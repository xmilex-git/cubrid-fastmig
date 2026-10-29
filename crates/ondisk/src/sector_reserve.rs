//! Sector reservation across the permanent data volumes, growing the database when they are full.
//!
//! Input: the volume set (read-write), the number of sectors a file needs. Output: that many sectors set in the
//! STABs, chosen as the engine's disk manager chooses them; when the free sectors do not cover the request, the
//! last volume is expanded up to its maximum and then volumes are added ([`crate::volume_expansion`],
//! [`crate::volume_addition`]).
//! The engine's disk cache (free sectors per volume, the allocation hint) is kept here in memory; it is loaded from
//! the STABs on first use, as the engine loads it at boot.
//! Source: engine `src/storage/disk_manager.c` `disk_reserve_sectors` (:4306), `disk_reserve_from_cache` (:4484),
//! `disk_reserve_from_cache_vols` (:4633), `disk_reserve_sectors_in_volume` (:4076), `disk_stab_unit_reserve`,
//! `disk_extend` (:1633), `disk_cache_load_volume` (:2567), `disk_unreserve_ordered_sectors`; develop 682b080b2.
//! Decision: a target that runs out of space grows exactly as the engine would have grown it (workspace#500,
//! ADR 0013).

use std::io;

use crate::ids::Vsid;
use crate::sector_table::{self, SECTOR_NPAGES};
use crate::volume_addition::{self, VolumePlace};
use crate::volume_expansion;
use crate::volume_set::VolumeSet;

/// Engine `DISK_MIN_VOLUME_SECTS`.
const MIN_VOLUME_SECTS: i32 = 64;

/// How the target grows: the engine parameter and paths a server on the target would use.
#[derive(Clone, Debug)]
pub struct VolumeGrowth {
    /// Engine `db_volume_size` in sectors (`nsect_vol_max`): size cap of an added volume.
    pub nsect_vol_max: i32,
    /// `None`: next to volume 0, no `_vinf` line.
    pub place: Option<VolumePlace>,
}

impl VolumeGrowth {
    /// `db_volume_size` = 512 MiB, the engine default.
    pub fn engine_default(io_pagesize: usize) -> Self {
        VolumeGrowth {
            nsect_vol_max: sectors_of_bytes(io_pagesize, 512 << 20),
            place: None,
        }
    }
}

/// Engine `DISK_SECTS_ROUND_UP (bytes / IO_SECTORSIZE)`.
pub fn sectors_of_bytes(io_pagesize: usize, bytes: u64) -> i32 {
    sector_table::round_up_sectors((bytes / (SECTOR_NPAGES as u64 * io_pagesize as u64)) as i32)
}

/// The engine's disk cache for permanent data volumes.
#[derive(Debug)]
pub struct DiskCache {
    /// Per volid: counts toward permanent data (purpose and type permanent).
    data: Vec<bool>,
    /// Per volid: sectors below `nsect_total` whose STAB bit is clear.
    free: Vec<i32>,
    /// Per volid: `hint_allocsect`, where the next search in the volume starts.
    hint: Vec<i32>,
    growth: VolumeGrowth,
}

impl DiskCache {
    /// Engine `disk_cache_load_volume` over every volume.
    pub fn load(vols: &VolumeSet, growth: VolumeGrowth) -> io::Result<Self> {
        let mut s = DiskCache {
            data: Vec::new(),
            free: Vec::new(),
            hint: Vec::new(),
            growth,
        };
        for v in vols.volumes() {
            let used = sector_table::used_sectors(vols, v.volid)?.len() as i32;
            s.data.push(v.header.purpose == 0 && v.header.vol_type == 0);
            s.free.push(v.header.nsect_total - used);
            s.hint.push(v.header.hint_allocsect);
        }
        Ok(s)
    }

    fn total_free(&self) -> i64 {
        self.data_vols().map(|v| self.free[v] as i64).sum()
    }

    fn data_vols(&self) -> impl DoubleEndedIterator<Item = usize> + '_ {
        (0..self.data.len()).filter(|&v| self.data[v])
    }

    /// Engine `disk_reserve_sectors` of permanent data sectors: `n` sectors set in the STABs. The engine takes the
    /// file's `volid_hint` but does not use it; neither does this.
    pub fn reserve(&mut self, vols: &VolumeSet, n: i32) -> io::Result<Vec<Vsid>> {
        let mut plan: Vec<(usize, i32)> = Vec::new();
        let mut remaining = n;
        if self.total_free() > remaining as i64 {
            self.take_from_vols(n, &mut remaining, &mut plan);
        }
        if remaining > 0 {
            // disk_reserve_from_cache rechecks the cache under the extend lock; nothing changes it in between here
            self.extend(vols, remaining, &mut remaining, &mut plan)?;
        }
        let mut out = Vec::with_capacity(n as usize);
        for (volid, count) in plan {
            self.take_in_volume(vols, volid, count, &mut out)?;
        }
        Ok(out)
    }

    /// Engine `disk_reserve_from_cache_vols`: volumes in volid order, skipping those with fewer than half the
    /// request free (avoid fragmentation).
    fn take_from_vols(&mut self, n: i32, remaining: &mut i32, plan: &mut Vec<(usize, i32)>) {
        let min_free = (n.min(self.growth.nsect_vol_max) / 2).max(1);
        for v in self.data_vols().collect::<Vec<_>>() {
            if *remaining == 0 {
                break;
            }
            if self.free[v] >= min_free {
                self.take_from_volume(v, remaining, plan);
            }
        }
    }

    /// Engine `disk_reserve_from_cache_volume`.
    fn take_from_volume(&mut self, v: usize, remaining: &mut i32, plan: &mut Vec<(usize, i32)>) {
        let k = self.free[v].min(*remaining);
        if k > 0 {
            self.free[v] -= k;
            *remaining -= k;
            plan.push((v, k));
        }
    }

    /// Engine `disk_extend` for permanent data: wanted = max(1 % of the total, 64) minus the free sectors, plus
    /// the request still unserved; first the expandable volume up to its maximum, then new volumes of up to
    /// `nsect_vol_max` sectors each.
    fn extend(
        &mut self,
        vols: &VolumeSet,
        intention: i32,
        remaining: &mut i32,
        plan: &mut Vec<(usize, i32)>,
    ) -> io::Result<()> {
        let heads: Vec<_> = vols.volumes();
        let (mut total, mut max) = (0i64, 0i64);
        for v in self.data_vols() {
            total += heads[v].header.nsect_total as i64;
            max += heads[v].header.nsect_max as i64;
        }
        let target_free = ((total as f64 * 0.01) as i64).max(MIN_VOLUME_SECTS as i64);
        let mut ext = (target_free - self.total_free()).max(0) + intention as i64;
        if ext <= 0 {
            return Ok(());
        }
        if total < max {
            let v = self
                .data_vols()
                .rfind(|&v| heads[v].header.nsect_total < heads[v].header.nsect_max)
                .ok_or_else(|| io::Error::other("no volume below its maximum"))?;
            // the engine keeps one such volume (volid_extend); its own room bounds the expansion
            let room = (heads[v].header.nsect_max - heads[v].header.nsect_total) as i64;
            let to_expand = ext.min(max - total).min(room) as i32;
            let got = volume_expansion::expand_volume(vols, v as i16, to_expand)?;
            self.free[v] += got;
            self.take_from_volume(v, remaining, plan);
            ext -= got as i64;
        }
        let vol_max = sector_table::round_up_sectors(self.growth.nsect_vol_max);
        let sys = sector_table::sys_nsect(vols.io_pagesize(), vol_max);
        while ext > 0 {
            let nsect_total = (ext.min((vol_max - sys) as i64) as i32 + sys).max(MIN_VOLUME_SECTS);
            let place = match &self.growth.place {
                Some(p) => p.clone(),
                None => VolumePlace::beside_first(vols)?,
            };
            let (volid, free) = volume_addition::add_volume(vols, nsect_total, vol_max, &place)?;
            let v = volid as usize;
            debug_assert_eq!(v, self.free.len());
            self.data.push(true);
            self.free.push(free);
            self.hint.push(-1);
            self.take_from_volume(v, remaining, plan);
            ext -= free as i64;
        }
        if *remaining > 0 {
            return Err(io::Error::other(format!(
                "disk extension left {remaining} sectors unreserved"
            )));
        }
        Ok(())
    }

    /// Engine `disk_reserve_sectors_in_volume`: `count` clear STAB bits of the volume, from the hint to
    /// `nsect_total` and then from the start to the hint, set and pushed to `out`; the hint moves past the last.
    fn take_in_volume(
        &mut self,
        vols: &VolumeSet,
        v: usize,
        count: i32,
        out: &mut Vec<Vsid>,
    ) -> io::Result<()> {
        let vh = vols.volume(v as i16)?.header.clone();
        let total = vh.nsect_total;
        let h = self.hint[v];
        let start = if h > 0 && h < total { h } else { 0 };
        let mut left = count;
        let mut last = -1;
        for (from, to) in [(start, total), (0, start)] {
            let mut s = from;
            while s < to && left > 0 {
                let (vpid, _, _) = sector_table::bit_position(&vh, vols.io_pagesize(), s);
                let page_first = (vpid.pageid - vh.stab_first_page)
                    * sector_table::sectors_per_page(vols.io_pagesize());
                let page_end =
                    (page_first + sector_table::sectors_per_page(vols.io_pagesize())).min(to);
                let mut p = vols.read_page(vpid)?;
                let mut changed = false;
                while s < page_end && left > 0 {
                    let (_, unit, bit) = sector_table::bit_position(&vh, vols.io_pagesize(), s);
                    if !sector_table::is_set(&p, unit, bit) {
                        sector_table::set_bit(&mut p, unit, bit, true);
                        out.push(Vsid::new(v as i16, s));
                        changed = true;
                        last = s;
                        left -= 1;
                    }
                    s += 1;
                }
                if changed {
                    vols.write_page(vpid, &p)?;
                }
            }
        }
        if left > 0 {
            return Err(io::Error::other(format!(
                "volume {v}: the cache promised {count} free sectors, the STAB has {}",
                count - left
            )));
        }
        // the engine keeps the hint STAB-unit aligned
        self.hint[v] = (last + 1) / 64 * 64;
        Ok(())
    }

    /// Engine `disk_unreserve_ordered_sectors` of one sector: its STAB bit cleared, the volume's free count up.
    /// Refuses a sector that is not reserved.
    pub fn release(&mut self, vols: &VolumeSet, vsid: Vsid) -> io::Result<()> {
        let vh = vols.volume(vsid.volid)?.header.clone();
        let (vpid, unit, bit) = sector_table::bit_position(&vh, vols.io_pagesize(), vsid.sectid);
        let mut p = vols.read_page(vpid)?;
        if !sector_table::set_bit(&mut p, unit, bit, false) {
            return Err(io::Error::other(format!(
                "sector {}|{} released but not reserved",
                vsid.volid, vsid.sectid
            )));
        }
        vols.write_page(vpid, &p)?;
        self.free[vsid.volid as usize] += 1;
        Ok(())
    }
}

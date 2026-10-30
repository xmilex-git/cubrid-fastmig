# partition-streams-102 — 10.2 partition expressions in every opcode range

A small fixture for the 10.2 partition stream transcoding of T17
[workspace#540](https://github.com/xmilex-git/workspace/issues/540): the same DDL and rows built on 10.2 and on
develop, so `cubrid-fastmig partition-streams --from 10.2` compares each transcoded 10.2 stream with the one develop
compiles. The T4 10.2 baseline DBs are not rebuilt for it (basedb/README.md: a rebuild changes the recorded LSAs).
Measured on 10.2.18.9024 → develop 682b080b2 (release builds).

## Run

```sh
bash run.sh <10.2 install copy> <develop install copy> <out dir>
cubrid-fastmig partition-streams <out>/db/x102ps/x102ps <out>/db/xdevps/xdevps --from 10.2 --out <dir>
```

Reflink copies of `/home/cubrid/release/CUBRID-fastmig-{10.2.18,dev}`; the script writes their `conf/cubrid.conf`,
runs in SA mode (no port) and leaves both DBs (about 300 MB) for the caller to delete.

## Tables (`10_partition_fx_dba.sql`, owner DBA)

| Table | Expression | What it covers (10.2 → develop opcode) |
|---|---|---|
| p_extract | `HASH(EXTRACT(YEAR FROM d))` | +1; 10.2 `T_EXTRACT` 114 is 11.4 `T_CASE`, so no pred offset (R13 V1) |
| p_floor | `RANGE(FLOOR(i / 10))` | +1 |
| p_ascii_hash | `HASH(ASCII(c))` | +5 |
| p_new_time | `RANGE(HOUR(NEW_TIME(dt, ..)))` | +7 (187 → 194) |
| p_from_tz | `HASH(TO_CHAR(FROM_TZ(dt, ..)))` | +7 (188 → 195); TO_CHAR's language flag |
| p_conv_tz | `RANGE(HOUR(CAST(dt AS DATETIME)))` | explicit and implicit CAST |
| p_char_hash, p_char_list | `CONCAT('x', c)`, c `CHAR(4)` | 10.2 compiles a built-in CHAR result, develop VARCHAR; values end in the padding |
| p_curdate | `HASH(DATEDIFF(d, CURRENT_DATE))` | R13 V6; develop refuses this DDL |
| p_benchmark | `HASH(CONCAT(c, BENCHMARK(1, 1)))` | R13 V3; 10.2 accepts it, the stream keeps `F_BENCHMARK` 963 |

## Results

- Trees equal, written bytes equal: p_extract, p_floor, p_ascii_hash, p_from_tz. p_from_tz needs the language flag
  remap: 10.2..11.3 store TO_CHAR/TO_DATE-family flags without the parity develop checks since CBRD-25302.
- Trees differ only where develop compiles differently: p_char_hash/p_char_list (CHAR vs VARCHAR result domain),
  p_new_time/p_conv_tz (10.2 `T_CAST` where develop writes the implicit cast as `T_CAST_WRAP`). The transcoder keeps
  the 10.2 tree; develop evaluates it to the same partition for every row (workspace#540 V4).
- p_benchmark: the transcoder refuses function code 963; the plan refuses the class by name.
- p_curdate: `CURRENT_DATE` stays in the stream (develop 202), not folded.

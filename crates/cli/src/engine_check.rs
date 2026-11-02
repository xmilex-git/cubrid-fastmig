//! `--engine-check`: the target engine reads the migrated target in SA mode, after the required SQL ran, and must
//! agree with the tool: every class's row count, and every index answering the same rows as a heap scan.
//!
//! Input: the target's SA runner and name, the tool's live rows per class (`class_live_rows`, counted before the
//! engine opened the target). Output: [`EngineCheck`]: classes whose counts differ or exist on one side only; per
//! index passed, failed (no such count, an error) or skipped with the reason.
//! Row counts (decision 3 (b)): `SELECT COUNT(*) FROM ONLY [owner].[class]` per user class (`db_class`, not a view);
//! the engine counts a partitioned class together with its partitions (`db_partition`), so the tool's side adds them.
//! Index checks (decision 3 (c)): per index of a user class that is not a partition (a partitioned class's query
//! covers its partitions' local indexes) the first key's minimum under the index's filter, then `key >= min` with
//! `USING INDEX [idx]` must plan an index scan of that index and count what `USING INDEX NONE` counts. A key the
//! engine does not range-scan gets `key = min`; when neither plans an index scan the index is skipped, not passed:
//! the develop engine plans no index scan for a `utf8_en_ci` column under any predicate, nor `>=` on an ENUM, on the
//! engine-built baseline DB too (workspace#611 scratch/611 p2, 1번 session decision 2026-11-02). Skipped too: an index
//! that is not `NORMAL INDEX` and one whose key has no non-NULL value.
//! Source: workspace#611 decision 3; csql facts in `csql_output`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io;

use crate::csql_output::{first_error, index_scans, rows as csql_rows, tagged_rows};
use crate::engine_sa::EngineSa;

pub struct ClassCount {
    pub class: String,
    pub tool: Option<u64>,
    pub engine: Option<u64>,
}

pub struct IndexOutcome {
    /// `owner.class.index`.
    pub index: String,
    pub result: IndexResult,
}

pub enum IndexResult {
    Passed { predicate: &'static str, rows: u64 },
    Skipped(String),
    Failed(String),
}

pub struct EngineCheck {
    pub classes_checked: usize,
    /// Classes whose counts differ, or that only one side has.
    pub class_differences: Vec<ClassCount>,
    pub indexes: Vec<IndexOutcome>,
    /// A catalog query that failed: the check could not list what to check.
    pub error: Option<String>,
}

impl EngineCheck {
    pub fn passed(&self) -> bool {
        self.error.is_none()
            && self.class_differences.is_empty()
            && !self
                .indexes
                .iter()
                .any(|i| matches!(i.result, IndexResult::Failed(_)))
    }

    /// (passed, failed, skipped by reason).
    pub fn index_counts(&self) -> (usize, usize, BTreeMap<String, usize>) {
        let (mut p, mut f, mut s) = (0, 0, BTreeMap::new());
        for i in &self.indexes {
            match &i.result {
                IndexResult::Passed { .. } => p += 1,
                IndexResult::Failed(_) => f += 1,
                IndexResult::Skipped(r) => *s.entry(r.clone()).or_insert(0) += 1,
            }
        }
        (p, f, s)
    }
}

/// `[a].[b]`, the way every generated statement names a class.
fn qualified(owner: &str, class: &str) -> String {
    format!("[{}].[{class}]", owner.to_lowercase())
}

/// The tool's name of a class: the owner-qualified name its class object stores.
fn tool_name(owner: &str, class: &str) -> String {
    format!("{}.{class}", owner.to_lowercase())
}

/// Rows of one catalog query; a failed query is the check's error.
fn rows(
    engine: &EngineSa,
    db: &str,
    name: &str,
    sql: &str,
) -> io::Result<Result<Vec<Vec<Option<String>>>, String>> {
    let out = engine.csql_query(name, db, sql)?;
    if let Some(e) = first_error(&out.err)? {
        return Ok(Err(format!("{name}: {} ({})", e.text, out.err.display())));
    }
    Ok(Ok(csql_rows(out.out.as_deref().unwrap_or(&out.log))?))
}

fn field(r: &[Option<String>], i: usize) -> String {
    r.get(i).cloned().flatten().unwrap_or_default()
}

pub fn run(
    engine: &EngineSa,
    db: &str,
    tool_rows: &BTreeMap<String, u64>,
) -> io::Result<EngineCheck> {
    let mut check = EngineCheck {
        classes_checked: 0,
        class_differences: Vec::new(),
        indexes: Vec::new(),
        error: None,
    };
    let classes = match rows(
        engine,
        db,
        "engine-classes",
        "SELECT owner_name, class_name FROM db_class WHERE is_system_class = 'NO' AND class_type = 'CLASS' ORDER BY 1, 2;\n",
    )? {
        Ok(r) => r,
        Err(e) => {
            check.error = Some(e);
            return Ok(check);
        }
    };
    let partitions = match rows(
        engine,
        db,
        "engine-partitions",
        "SELECT owner_name, class_name, partition_class_name FROM db_partition ORDER BY 1, 2;\n",
    )? {
        Ok(r) => r,
        Err(e) => {
            check.error = Some(e);
            return Ok(check);
        }
    };
    // partitioned class (tool name) → its partitions (tool names); every partition (tool name)
    let mut parts_of: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut is_partition = BTreeSet::new();
    for r in &partitions {
        let (o, c, p) = (field(r, 0), field(r, 1), field(r, 2));
        parts_of
            .entry(tool_name(&o, &c))
            .or_default()
            .push(tool_name(&o, &p));
        is_partition.insert(tool_name(&o, &p));
    }

    // (b) row counts
    let mut sql = String::new();
    for (k, r) in classes.iter().enumerate() {
        let _ = writeln!(
            sql,
            "SELECT 'c{k}', COUNT(*) FROM ONLY {};",
            qualified(&field(r, 0), &field(r, 1))
        );
    }
    let out = engine.csql_query("engine-class-counts", db, &sql)?;
    let counted: BTreeMap<String, u64> = tagged_rows(out.out.as_deref().unwrap_or(&out.log), "c")?
        .into_iter()
        .filter_map(|(tag, v)| Some((tag, v.first()?.as_deref()?.parse().ok()?)))
        .collect();
    let mut engine_names = BTreeSet::new();
    for (k, r) in classes.iter().enumerate() {
        let name = tool_name(&field(r, 0), &field(r, 1));
        engine_names.insert(name.clone());
        let own = tool_rows.get(&name).copied();
        let tool = own.map(|n| {
            n + parts_of
                .get(&name)
                .into_iter()
                .flatten()
                .filter_map(|p| tool_rows.get(p))
                .sum::<u64>()
        });
        let engine = counted.get(&format!("c{k}")).copied();
        check.classes_checked += 1;
        if tool != engine || tool.is_none() {
            check.class_differences.push(ClassCount {
                class: name,
                tool,
                engine,
            });
        }
    }
    // a user class of the tool (owner-qualified) the engine does not list
    for (name, &n) in tool_rows {
        if name.contains('.') && !engine_names.contains(name) {
            check.class_differences.push(ClassCount {
                class: name.clone(),
                tool: Some(n),
                engine: None,
            });
        }
    }

    // (c) index checks
    // two queries joined here: an outer join of the catalog views fails on develop ("Outer join query optimization
    // failed", workspace#611 run-01)
    let attributes = match rows(
        engine,
        db,
        "engine-attributes",
        "SELECT a.owner_name, a.class_name, a.attr_name, a.data_type, a.collation FROM db_attribute a, db_class c \
         WHERE c.class_name = a.class_name AND c.owner_name = a.owner_name AND c.is_system_class = 'NO';\n",
    )? {
        Ok(r) => r,
        Err(e) => {
            check.error = Some(e);
            return Ok(check);
        }
    };
    let key_type: BTreeMap<(String, String, String), (String, String)> = attributes
        .iter()
        .map(|r| {
            (
                (field(r, 0), field(r, 1), field(r, 2)),
                (field(r, 3), field(r, 4)),
            )
        })
        .collect();
    let mut indexes = match rows(
        engine,
        db,
        "engine-indexes",
        "SELECT k.owner_name, k.class_name, k.index_name, k.key_attr_name, k.func, i.filter_expression, i.status \
         FROM db_index_key k, db_index i, db_class c \
         WHERE k.key_order = 0 AND i.index_name = k.index_name AND i.class_name = k.class_name \
         AND i.owner_name = k.owner_name AND c.class_name = k.class_name AND c.owner_name = k.owner_name \
         AND c.is_system_class = 'NO' ORDER BY 1, 2, 3;\n",
    )? {
        Ok(r) => r,
        Err(e) => {
            check.error = Some(e);
            return Ok(check);
        }
    };
    // fields 7 and 8: the first key's data type and collation
    for r in &mut indexes {
        let (t, c) = key_type
            .get(&(field(r, 0), field(r, 1), field(r, 3)))
            .cloned()
            .unwrap_or_default();
        r.extend([Some(t), Some(c)]);
    }
    check.indexes = index_checks(engine, db, &indexes, &is_partition)?;
    Ok(check)
}

/// One index to check: its table, key expression, filter and why it would be skipped.
struct IndexUnderCheck {
    name: String,
    index: String,
    table: String,
    /// The table as a plan names it (`app_owner.customer`).
    plan_table: String,
    key: String,
    filter: Option<String>,
    key_kind: String,
}

fn index_checks(
    engine: &EngineSa,
    db: &str,
    indexes: &[Vec<Option<String>>],
    is_partition: &BTreeSet<String>,
) -> io::Result<Vec<IndexOutcome>> {
    let mut out = Vec::new();
    let mut todo = Vec::new();
    for r in indexes {
        let (owner, class, index) = (field(r, 0), field(r, 1), field(r, 2));
        if is_partition.contains(&tool_name(&owner, &class)) {
            continue;
        }
        let name = format!("{}.{index}", tool_name(&owner, &class));
        if field(r, 6) != "NORMAL INDEX" {
            out.push(IndexOutcome {
                index: name,
                result: IndexResult::Skipped(format!("status {}", field(r, 6))),
            });
            continue;
        }
        let func = field(r, 4);
        let (key, key_kind) = if func.trim().is_empty() {
            let collation = field(r, 8);
            let kind = match collation.as_str() {
                "" | "Not applicable" => field(r, 7),
                c => format!("{} {c}", field(r, 7)),
            };
            (format!("[{}]", field(r, 3)), kind)
        } else {
            (func.trim().to_string(), "function key".to_string())
        };
        todo.push(IndexUnderCheck {
            name,
            index,
            table: qualified(&owner, &class),
            plan_table: tool_name(&owner, &class),
            key,
            filter: Some(field(r, 5)).filter(|f| !f.trim().is_empty()),
            key_kind,
        });
    }
    let filter_and = |i: &IndexUnderCheck| {
        i.filter
            .as_ref()
            .map_or(String::new(), |f| format!("({f}) AND "))
    };

    // the first key's minimum under the filter
    let mut sql = String::new();
    for (k, i) in todo.iter().enumerate() {
        let wher = i
            .filter
            .as_ref()
            .map_or(String::new(), |f| format!(" WHERE {f}"));
        let _ = writeln!(sql, "SELECT 'm{k}', MIN({}) FROM {}{wher};", i.key, i.table);
    }
    let min_run = engine.csql_query("engine-index-min", db, &sql)?;
    let mins: BTreeMap<String, Option<String>> =
        tagged_rows(min_run.out.as_deref().unwrap_or(&min_run.log), "m")?
            .into_iter()
            .map(|(tag, v)| (tag, v.into_iter().next().flatten()))
            .collect();

    // `>=` first, then `=` for the keys that planned no index scan
    let mut open: Vec<(usize, String)> = Vec::new();
    for (k, i) in todo.iter().enumerate() {
        match mins.get(&format!("m{k}")) {
            None => out.push(IndexOutcome {
                index: i.name.clone(),
                result: IndexResult::Failed("MIN of the key failed (engine-index-min.err)".into()),
            }),
            Some(None) => out.push(IndexOutcome {
                index: i.name.clone(),
                result: IndexResult::Skipped("no non-NULL key".into()),
            }),
            Some(Some(m)) => open.push((k, m.clone())),
        }
    }
    let mut decided: BTreeMap<usize, IndexResult> = BTreeMap::new();
    for (round, op) in [(0, ">="), (1, "=")] {
        let mut sql = String::from(";plan simple\n");
        for (k, min) in &open {
            let i = &todo[*k];
            let wher = format!("{}{} {op} {min}", filter_and(i), i.key);
            let _ = writeln!(
                sql,
                "SELECT 'x{k}', COUNT(*) FROM {} WHERE {wher} USING INDEX [{}];",
                i.table, i.index
            );
            let _ = writeln!(
                sql,
                "SELECT 'n{k}', COUNT(*) FROM {} WHERE {wher} USING INDEX NONE;",
                i.table
            );
        }
        let run = engine.csql_query(
            &format!("engine-index-{}", ["range", "equal"][round]),
            db,
            &sql,
        )?;
        let got: BTreeMap<String, Option<u64>> =
            tagged_rows(run.out.as_deref().unwrap_or(&run.log), "")?
                .into_iter()
                .map(|(tag, v)| {
                    (
                        tag,
                        v.first().cloned().flatten().and_then(|v| v.parse().ok()),
                    )
                })
                .collect();
        // csql prints plans on stdout, apart from the rows (-o): a plan is matched by its table and index, which
        // one round's queries name once each
        let plans = index_scans(&run.log)?;
        let mut next = Vec::new();
        for (k, min) in open {
            let i = &todo[k];
            let (x, n) = (got.get(&format!("x{k}")), got.get(&format!("n{k}")));
            let head = format!("Index scan({} {}, {},", i.plan_table, i.plan_table, i.index);
            let scans = plans.iter().any(|p| p.starts_with(&head));
            let result = match (x, n) {
                (Some(Some(a)), Some(Some(b))) if scans && a == b => IndexResult::Passed {
                    predicate: op,
                    rows: *a,
                },
                (Some(Some(a)), Some(Some(b))) if scans => IndexResult::Failed(format!(
                    "key {op} {min}: index scan {a} rows, heap scan {b}"
                )),
                (Some(_), Some(_)) if round == 0 => {
                    next.push((k, min));
                    continue;
                }
                (Some(_), Some(_)) => {
                    IndexResult::Skipped(format!("no index scan for its key: {}", i.key_kind))
                }
                _ => IndexResult::Failed(format!(
                    "the count query failed (engine-index-{}.err)",
                    ["range", "equal"][round]
                )),
            };
            decided.insert(k, result);
        }
        open = next;
    }
    for (k, result) in decided {
        out.push(IndexOutcome {
            index: todo[k].name.clone(),
            result,
        });
    }
    Ok(out)
}

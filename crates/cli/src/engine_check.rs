//! `--engine-check`: the target engine reads the migrated target in SA mode, after the required SQL ran, and must
//! agree with the tool: every class's row count, and every index answering the same rows as a heap scan.
//!
//! Input: the target's SA runner and name, the tool's live rows per class (`class_live_rows`, counted before the
//! engine opened the target). Output: [`EngineCheck`]: classes whose counts differ or exist on one side only; per
//! index passed, failed (no such count, an error) or skipped with the reason.
//! Row counts (decision 3 (b)): `COUNT(*) FROM ONLY [owner].[class]` per user class (`db_class`, not a view);
//! the engine counts a partitioned class together with its partitions (`db_partition`), so the tool's side adds them.
//! Index checks (decision 3 (c)): per index of a user class that is not a partition (a partitioned class's query
//! covers its partitions' local indexes) the first key's minimum under the index's filter, then `key >= min` with
//! `USING INDEX [idx]` must plan an index scan of that index and count what `USING INDEX NONE` counts. The heap side
//! of every index and the class's row count come from one heap scan per class that really reads the heap
//! ([`heap_counts`], workspace#613); the index side stays one query per index and runs first, because its plans
//! decide which count each index needs. Every query names the class `FROM ONLY`, as the row count does (a
//! partitioned class still covers its partitions). A key the engine does not range-scan gets `key = min`; when
//! neither plans an index scan the index is skipped, not passed: the develop engine plans no index scan for a
//! `utf8_en_ci` column under any predicate, nor `>=` on an ENUM, on the engine-built baseline DB too (workspace#611
//! scratch/611 p2, 1번 session decision 2026-11-02). Skipped too: an index that is not `NORMAL INDEX` and one whose
//! key has no non-NULL value.
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
    let (skipped, todo) = indexes_under_check(&indexes, &is_partition);
    check.indexes = skipped;
    let (mut outcomes, open) = index_minimums(engine, db, &todo)?;
    check.indexes.append(&mut outcomes);
    // (c) the index side first: its plans decide which count each index needs from the heap side
    let (mut decided, scanned) = index_side(engine, db, &todo, open)?;

    // (b) row counts, with the heap side of (c) in the same scan
    let heap = heap_counts(engine, db, &classes, &todo, &scanned)?;
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
        let engine = heap.rows.get(&k).copied();
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

    // (c) the index scan's count against the heap's
    for s in scanned {
        let result = match heap.index_rows.get(&s.index) {
            Some(&b) if b == s.rows => IndexResult::Passed {
                predicate: s.op,
                rows: s.rows,
            },
            Some(b) => IndexResult::Failed(format!(
                "key {} {}: index scan {} rows, heap scan {b}",
                s.op, s.min, s.rows
            )),
            None => IndexResult::Failed(
                "the heap count of the class failed (engine-heap-counts.err)".into(),
            ),
        };
        decided.insert(s.index, result);
    }
    for (k, result) in decided {
        check.indexes.push(IndexOutcome {
            index: todo[k].name.clone(),
            result,
        });
    }
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
    /// The key is a column whose minimum csql prints exactly: not FLOAT, DOUBLE or MONETARY (`-q -N` prints those
    /// as `%.15e`, which can round) and not a function key.
    exact_minimum: bool,
}

impl IndexUnderCheck {
    /// The index's filter and the key compared with `min` by `op`: the predicate the index side counts.
    fn predicate(&self, op: &str, min: &str) -> String {
        let filter_and = self
            .filter
            .as_ref()
            .map_or(String::new(), |f| format!("({f}) AND "));
        format!("{filter_and}{} {op} {min}", self.key)
    }

    /// The heap side's aggregate for the index side's `predicate(op, min)`. With an exactly printed minimum,
    /// `key >= min` under the filter is every non-NULL key under the filter, so a plain `COUNT(key)` counts it: a
    /// `CASE` column costs about as much as one more heap scan, `COUNT(key)` next to nothing (workspace#613
    /// scratch/613 probe/oi-*).
    fn heap_aggregate(&self, op: &str, min: &str) -> String {
        match (op, &self.filter) {
            (">=", None) if self.exact_minimum => format!("COUNT({})", self.key),
            (">=", Some(f)) if self.exact_minimum => {
                format!("COUNT(CASE WHEN ({f}) THEN {} END)", self.key)
            }
            _ => format!("COUNT(CASE WHEN {} THEN 1 END)", self.predicate(op, min)),
        }
    }
}

/// The indexes to check, and the outcomes of those skipped up front (not `NORMAL INDEX`). A partition's index is
/// left out: its partitioned class's query covers its partitions' local indexes.
fn indexes_under_check(
    indexes: &[Vec<Option<String>>],
    is_partition: &BTreeSet<String>,
) -> (Vec<IndexOutcome>, Vec<IndexUnderCheck>) {
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
            exact_minimum: func.trim().is_empty()
                && !matches!(field(r, 7).as_str(), "FLOAT" | "DOUBLE" | "MONETARY"),
        });
    }
    (out, todo)
}

/// An index still to check: (index in `todo`, its first key's minimum as a literal).
type OpenIndex = (usize, String);

/// The first key's minimum under the filter per index: the outcomes of the indexes that end here (the query failed,
/// no non-NULL key) and the rest still open.
fn index_minimums(
    engine: &EngineSa,
    db: &str,
    todo: &[IndexUnderCheck],
) -> io::Result<(Vec<IndexOutcome>, Vec<OpenIndex>)> {
    let mut sql = String::new();
    for (k, i) in todo.iter().enumerate() {
        let wher = i
            .filter
            .as_ref()
            .map_or(String::new(), |f| format!(" WHERE {f}"));
        let _ = writeln!(
            sql,
            "SELECT 'm{k}', MIN({}) FROM ONLY {}{wher};",
            i.key, i.table
        );
    }
    let run = engine.csql_query("engine-index-min", db, &sql)?;
    let mins: BTreeMap<String, Option<String>> =
        tagged_rows(run.out.as_deref().unwrap_or(&run.log), "m")?
            .into_iter()
            .map(|(tag, v)| (tag, v.into_iter().next().flatten()))
            .collect();
    let (mut out, mut open) = (Vec::new(), Vec::new());
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
    Ok((out, open))
}

/// An index whose `USING INDEX` query planned an index scan of it: what it counted, to compare with the heap side.
struct Scanned {
    /// Index in `todo`.
    index: usize,
    op: &'static str,
    min: String,
    rows: u64,
}

/// Per open index, `COUNT(*) … WHERE <predicate> USING INDEX [idx]` with `>=` first, then `=` for the keys that
/// planned no index scan. Returns the indexes decided here (skipped: no index scan for either; failed: the query
/// failed) and those that scanned.
fn index_side(
    engine: &EngineSa,
    db: &str,
    todo: &[IndexUnderCheck],
    mut open: Vec<OpenIndex>,
) -> io::Result<(BTreeMap<usize, IndexResult>, Vec<Scanned>)> {
    let mut decided = BTreeMap::new();
    let mut scanned = Vec::new();
    for (round, op) in [(0, ">="), (1, "=")] {
        if open.is_empty() {
            break;
        }
        let mut sql = String::from(";plan simple\n");
        for (k, min) in &open {
            let i = &todo[*k];
            let _ = writeln!(
                sql,
                "SELECT 'x{k}', COUNT(*) FROM ONLY {} WHERE {} USING INDEX [{}];",
                i.table,
                i.predicate(op, min),
                i.index
            );
        }
        let run_name = ["engine-index-range", "engine-index-equal"][round];
        let run = engine.csql_query(run_name, db, &sql)?;
        let got: BTreeMap<String, Option<u64>> =
            tagged_rows(run.out.as_deref().unwrap_or(&run.log), "x")?
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
            let head = format!("Index scan({} {}, {},", i.plan_table, i.plan_table, i.index);
            let scans = plans.iter().any(|p| p.starts_with(&head));
            match got.get(&format!("x{k}")).copied().flatten() {
                None => {
                    decided.insert(
                        k,
                        IndexResult::Failed(format!("the count query failed ({run_name}.err)")),
                    );
                }
                Some(rows) if scans => scanned.push(Scanned {
                    index: k,
                    op,
                    min,
                    rows,
                }),
                Some(_) if round == 0 => next.push((k, min)),
                Some(_) => {
                    decided.insert(
                        k,
                        IndexResult::Skipped(format!("no index scan for its key: {}", i.key_kind)),
                    );
                }
            }
        }
        open = next;
    }
    Ok((decided, scanned))
}

/// What one heap scan per class counted: its rows, and per scanned index the heap's count of its predicate.
struct HeapCounts {
    /// class (index in the class list) → rows
    rows: BTreeMap<usize, u64>,
    /// index in `todo` → rows
    index_rows: BTreeMap<usize, u64>,
}

/// One `USING INDEX NONE` scan per class (workspace#613 decision 1): `COUNT(*)` and, per scanned index of the class,
/// the aggregate of `IndexUnderCheck::heap_aggregate` (one column per distinct aggregate). The run is REPEATABLE
/// READ: under READ COMMITTED the engine answers a `COUNT(*)` without WHERE from the B-tree statistics without
/// reading the heap (develop `query_executor.c:qexec_evaluate_aggregates_optimize`; trace `noscan … readrows: 0`), and
/// those statistics are what the tool wrote (workspace#613 decision 2, scratch/613 probe/t1). `COUNT(col)` and
/// `COUNT(CASE …)` are never answered from an index (develop `xasl_generation.c:pt_to_aggregate_node`: only
/// `COUNT(*)`, `MIN` and `MAX`).
fn heap_counts(
    engine: &EngineSa,
    db: &str,
    classes: &[Vec<Option<String>>],
    todo: &[IndexUnderCheck],
    scanned: &[Scanned],
) -> io::Result<HeapCounts> {
    let mut sql = String::from("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ;\n");
    // per class: index in `todo` → column after COUNT(*)
    let mut columns: Vec<BTreeMap<usize, usize>> = Vec::new();
    for (c, r) in classes.iter().enumerate() {
        let table = qualified(&field(r, 0), &field(r, 1));
        let mut aggregates: Vec<String> = Vec::new();
        let mut cols = BTreeMap::new();
        for s in scanned.iter().filter(|s| todo[s.index].table == table) {
            let a = todo[s.index].heap_aggregate(s.op, &s.min);
            let col = match aggregates.iter().position(|x| *x == a) {
                Some(col) => col,
                None => {
                    aggregates.push(a);
                    aggregates.len() - 1
                }
            };
            cols.insert(s.index, col);
        }
        let mut select = format!("SELECT 'c{c}', COUNT(*)");
        for a in &aggregates {
            let _ = write!(select, ", {a}");
        }
        let _ = writeln!(sql, "{select} FROM ONLY {table} USING INDEX NONE;");
        columns.push(cols);
    }
    let run = engine.csql_query("engine-heap-counts", db, &sql)?;
    let mut heap = HeapCounts {
        rows: BTreeMap::new(),
        index_rows: BTreeMap::new(),
    };
    for (tag, v) in tagged_rows(run.out.as_deref().unwrap_or(&run.log), "c")? {
        let Some(c) = tag.strip_prefix('c').and_then(|c| c.parse::<usize>().ok()) else {
            continue;
        };
        let counts: Vec<Option<u64>> = v
            .iter()
            .map(|f| f.as_deref().and_then(|f| f.parse().ok()))
            .collect();
        if let Some(Some(n)) = counts.first() {
            heap.rows.insert(c, *n);
        }
        for (&k, &col) in columns.get(c).into_iter().flatten() {
            if let Some(Some(n)) = counts.get(col + 1) {
                heap.index_rows.insert(k, *n);
            }
        }
    }
    Ok(heap)
}

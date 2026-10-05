//! Concurrency runner and warmup loop.
//!
//! Each scenario implements `async fn run(cfg) -> Result<Iteration>`.
//! The runner does `warmup` iterations serially (ignoring results),
//! then runs `iters` iterations with `concurrency` in-flight at a time,
//! collecting per-iteration latencies for percentile stats.

use crate::metrics::Report;
use crate::scenarios::{self, Iteration, ScenarioConfig};
use anyhow::{Context, Result};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

pub async fn run(
    scenario: &str,
    concurrency: usize,
    iters: usize,
    warmup: usize,
    cfg: &ScenarioConfig,
) -> Result<Report> {
    if concurrency == 0 {
        anyhow::bail!("concurrency must be >= 1");
    }
    if iters == 0 {
        anyhow::bail!("iters must be >= 1");
    }

    // Warmup — serial, discards latencies and errors. We still surface
    // a failing warmup as an error because it usually means the scenario
    // can't run at all (missing cgroup perms, no user namespace, etc.).
    for i in 0..warmup {
        scenarios::dispatch(scenario, cfg)
            .await
            .with_context(|| format!("warmup iteration {i} of {scenario}"))?;
    }

    let sem = Arc::new(Semaphore::new(concurrency));
    let mut set: JoinSet<Iteration> = JoinSet::new();
    let scenario_name = scenario.to_string();
    let cfg_owned = cfg.clone();

    let start = Instant::now();
    for _ in 0..iters {
        let permit = sem.clone().acquire_owned().await.expect("semaphore closed");
        let name = scenario_name.clone();
        let cfg = cfg_owned.clone();
        set.spawn(async move {
            let _permit = permit;
            match scenarios::dispatch(&name, &cfg).await {
                Ok(it) => it,
                Err(e) => Iteration::from_err(e),
            }
        });
    }

    let mut latencies_us: Vec<u64> = Vec::with_capacity(iters);
    let mut errors = 0usize;
    let mut extras: Vec<serde_json::Value> = Vec::new();

    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(it) => {
                if it.ok {
                    latencies_us.push(it.duration_us);
                    if !it.extra.is_null() {
                        extras.push(it.extra);
                    }
                } else {
                    errors += 1;
                }
            }
            Err(_join_err) => {
                // Task panicked. Count as an error; the panic message
                // already went to stderr via the default panic hook.
                errors += 1;
            }
        }
    }

    let wall = start.elapsed();

    Ok(Report::from_samples(
        scenario,
        concurrency,
        iters,
        warmup,
        wall,
        &latencies_us,
        errors,
        merge_extras(extras),
    ))
}

/// Fold per-iteration extras into one document.
///
/// Most scenarios emit the same shape each iteration (e.g.
/// `clone_method: "reflink"`); in that case, return the single value.
///
/// If iterations emit objects that share keys but have varying numeric
/// values per key (typical for throughput/size fields), collapse each
/// numeric key to `{ min, p50, max }`. String keys that disagree are
/// surfaced as a per-value count. This keeps the JSON readable when
/// scenarios record derived metrics like `mb_per_sec`.
fn merge_extras(extras: Vec<serde_json::Value>) -> serde_json::Value {
    if extras.is_empty() {
        return serde_json::Value::Null;
    }
    if extras.len() == 1 || extras.iter().all(|e| e == &extras[0]) {
        return extras.into_iter().next().unwrap();
    }

    // All objects — collapse per-key.
    if extras.iter().all(|e| e.is_object()) {
        let mut out = serde_json::Map::new();
        let keys: std::collections::BTreeSet<String> = extras
            .iter()
            .flat_map(|e| e.as_object().unwrap().keys().cloned())
            .collect();
        for k in keys {
            let values: Vec<&serde_json::Value> = extras
                .iter()
                .filter_map(|e| e.as_object().unwrap().get(&k))
                .collect();
            out.insert(k, collapse_values(&values));
        }
        return serde_json::Value::Object(out);
    }

    // Non-object mixed — count distinct string reprs.
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for e in &extras {
        *counts.entry(e.to_string()).or_insert(0) += 1;
    }
    serde_json::to_value(counts).unwrap_or(serde_json::Value::Null)
}

fn collapse_values(values: &[&serde_json::Value]) -> serde_json::Value {
    if values.iter().all(|v| v == &values[0]) {
        return values[0].clone();
    }
    // Numeric? report min / p50 / max.
    let nums: Option<Vec<f64>> = values.iter().map(|v| v.as_f64()).collect();
    if let Some(mut nums) = nums {
        nums.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let n = nums.len();
        return serde_json::json!({
            "min": nums[0],
            "p50": nums[n / 2],
            "max": nums[n - 1],
        });
    }
    // Fallback: array of unique string reprs.
    let uniq: std::collections::BTreeSet<String> = values.iter().map(|v| v.to_string()).collect();
    serde_json::to_value(uniq).unwrap_or(serde_json::Value::Null)
}

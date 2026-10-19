use super::report::*;
use serde_json::{Value, json};
pub(super) fn fixture() -> Value {
    let records:Vec<_>=expected_ids().into_iter().map(|id|{
        let p:Vec<_>=id.split('/').collect();let canvas=p.first()==Some(&"canvas");let n=p.get(2).unwrap_or(&"0").parse::<u64>().unwrap_or(0);
        let nodes=if canvas{p.get(1).unwrap().parse().unwrap()}else{n};
        let shape=if canvas{"grid"}else{p.get(1).copied().unwrap()};let op=p.last().copied().unwrap();
        let kind=match p.first().copied().unwrap(){"graph"=>"graph_cpu","engine"=>"dispatch_cpu","canvas"=>"canvas_cpu",v=>v};
        let edges=if canvas {let cols=(1..=nodes).find(|n|n*n>=nodes).unwrap();nodes-nodes.div_ceil(cols)}else if shape=="disconnected"{0}else{nodes-1};
        let frames=if canvas&&op=="zoom"{160}else{120};let context=if canvas{json!({"ppp":p.get(2).unwrap().parse::<u64>().unwrap(),"viewport":[1600,1000],"capture":true,"workloads":vec![json!({"total_nodes":nodes,"visible_nodes":if nodes==200{200}else{10},"total_edges":edges,"visible_edges":0,"visibility_unavailable":null});frames]})}else{json!({"boundary":"fixture"})};
        let perf=if canvas{
            let metrics:Vec<_>=["frame_prepare","frame_interval","dispatch","graph_stage","graph_stage_refused","presentation","undo","redo","load","save"].into_iter().map(|name|{
                let count=if name=="frame_prepare"{frames}else{0};let q=if count>0{json!(1)}else{Value::Null};
                json!({"metric":name,"unit":"us","supported":name!="presentation","samples":count,"recorded":count,"invalid":0,"p50":q,"p95":q,"p99":q,"max":q,"unavailable_reason":if name=="presentation"{json!("presentation_timestamps_not_available")}else{Value::Null}})
            }).collect();json!({"schema":"nodecraft.perf/2","clock":true,"metrics":metrics,"workload":{"total_nodes":nodes},"environment":{"profile":"release"}})
        }else{Value::Null};
        json!({"id":id,"measurement":kind,"shape":shape,"nodes":nodes,"edges":edges,"operation":op,"warmups":3,"samples_ns":vec![100u64;frames],"context":context,"perf2":perf})
    }).collect();
    json!({"schema":SCHEMA,"workload_version":WORKLOAD,"definitions":"a".repeat(40),"source":{"revision":"b".repeat(40),"fingerprint":"c".repeat(40),"dirty":false},"environment":{"machine":"fixture-cpu","cpu_model":"fixture-cpu-model","flags_fingerprint":"1".repeat(40),"cargo":"cargo fixture","rustc":"rustc fixture","target":"fixture-target","os":"fixture","arch":"fixture","logical_cpus":2,"profile":"release"},"binaries":{"graph":"d".repeat(40),"engine":"e".repeat(40),"canvas":"f".repeat(40)},"records":records,"unsupported":["gpu_timing","presentation_timing","wasm_runtime"]})
}
#[test]
fn complete_matrix_equal_and_revision_change_compare() {
    let a = fixture();
    validate(&a).unwrap();
    assert_eq!(a["records"].as_array().unwrap().len(), 72);
    let mut b = a.clone();
    b["source"]["revision"] = json!("f".repeat(40));
    assert!(!compare(&a, &b).unwrap().0);
}
#[test]
fn tail_only_regression_is_not_hidden_by_same_median() {
    let a = fixture();
    let mut b = a.clone();
    let raw = b["records"][0]["samples_ns"].as_array_mut().unwrap();
    for n in raw.iter_mut().rev().take(3) {
        *n = json!(1000);
    }
    let (signal, md) = compare(&a, &b).unwrap();
    assert!(signal);
    assert!(md.contains("p99"));
    assert!(md.contains("regression signal"));
    assert_eq!(percentile(&samples(&a["records"][0]).unwrap(), 50), percentile(&samples(&b["records"][0]).unwrap(), 50));
}
#[test]
fn percentile_convention_and_zero_baseline_are_explicit() {
    let raw: Vec<_> = (1..=120).collect();
    assert_eq!(percentile(&raw, 50), Some(60));
    assert_eq!(percentile(&raw, 95), Some(114));
    assert_eq!(percentile(&raw, 99), Some(119));
    let mut a = fixture();
    for r in a["records"].as_array_mut().unwrap() {
        for n in r["samples_ns"].as_array_mut().unwrap() {
            *n = json!(0);
        }
    }
    assert!(!compare(&a, &a).unwrap().0);
    let mut b = a.clone();
    b["records"][0]["samples_ns"] = json!(vec![1; 120]);
    let (signal, md) = compare(&a, &b).unwrap();
    assert!(signal);
    assert!(md.contains("unavailable: zero baseline"));
}
#[test]
fn missing_duplicate_counts_under_sampled_and_unavailable_are_not_passes() {
    let a = fixture();
    let mut bad = a.clone();
    bad["records"].as_array_mut().unwrap().pop();
    assert!(validate(&bad).is_err());
    bad = a.clone();
    bad["records"][1] = bad["records"][0].clone();
    assert!(validate(&bad).is_err());
    bad = a.clone();
    bad["records"][0]["samples_ns"] = json!(vec![100; 9]);
    assert!(compare(&a, &bad).is_err());
    bad = a.clone();
    bad["records"][0]["samples_ns"] = json!([]);
    assert!(validate(&bad).is_err());
    bad = a.clone();
    let row = bad["records"].as_array_mut().unwrap().iter_mut().find(|r| r["measurement"] == "canvas_cpu").unwrap();
    row["context"]["workloads"][0]["visible_edges"] = Value::Null;
    assert!(validate(&bad).is_err());
    bad = a.clone();
    let row = bad["records"].as_array_mut().unwrap().iter_mut().find(|r| r["measurement"] == "canvas_cpu").unwrap();
    row["perf2"]["metrics"][0]["supported"] = json!(false);
    assert!(validate(&bad).is_err());
}
#[test]
fn environment_definition_and_visibility_drift_are_incomparable() {
    let a = fixture();
    for field in ["definitions", "environment"] {
        let mut b = a.clone();
        b[field] = if field == "definitions" {
            json!("e".repeat(40))
        } else {
            let mut e = b[field].clone();
            e["machine"] = json!("other-hardware");
            e
        };
        assert!(compare(&a, &b).is_err());
    }
    let mut b = a.clone();
    let r = b["records"].as_array_mut().unwrap().iter_mut().find(|r| r["measurement"] == "canvas_cpu" && r["nodes"] == 1000).unwrap();
    r["context"]["workloads"][0]["visible_nodes"] = json!(11);
    assert!(compare(&a, &b).is_err());
}
#[test]
fn hostile_json_is_rejected_before_large_tree_allocation() {
    assert!(parse(&vec![b' '; MAX_BYTES + 1]).is_err());
    assert!(parse(format!("{}0{}", "[".repeat(25), "]".repeat(25)).as_bytes()).is_err());
    assert!(parse(format!("\"{}\"", "x".repeat(4097)).as_bytes()).is_err());
    assert!(parse(b"{not json").is_err());
    let mut bad = fixture();
    bad["records"][0]["samples_ns"][0] = json!(-1);
    assert!(validate(&bad).is_err());
    bad["records"][0]["samples_ns"][0] = json!(3_600_000_000_001u64);
    assert!(validate(&bad).is_err());
}

#[test]
fn isolated_outlier_cannot_erase_a_body_regression_budget() {
    let mut a = fixture();
    a["records"][0]["samples_ns"][119] = json!(1_000_000);
    let mut b = a.clone();
    for n in b["records"][0]["samples_ns"].as_array_mut().unwrap().iter_mut().take(119) {
        *n = json!(200);
    }
    let (signal, md) = compare(&a, &b).unwrap();
    assert!(signal);
    assert!(md.contains(BUDGET_POLICY));
    let policy = budgets(&a).unwrap();
    assert_eq!(policy["workloads"][0]["allowance_ns"], json!(0));
    assert_eq!(policy["workloads"][0]["budgets_ns"]["p50"], json!(100));
    assert_eq!(policy["workloads"][0]["baseline_stats_ns"]["max"], json!(1_000_000));
}

#[test]
fn old_unrecorded_build_configuration_is_not_silently_upgraded() {
    let mut old = fixture();
    old["schema"] = json!("nodecraft.bench/1");
    assert!(validate(&old).is_err());
    let mut missing = fixture();
    missing["environment"].as_object_mut().unwrap().remove("cargo");
    assert!(validate(&missing).is_err());
}

#[test]
fn comparison_provenance_is_public_escaped_bounded_and_dirty_qualified() {
    let mut a = fixture();
    a["environment"]["cpu_model"] = json!("<script>[cpu](https://invalid) `code` | & *em*\n# heading");
    a["environment"]["cargo"] = json!("cargo /private/not-rendered");
    let mut b = a.clone();
    b["source"]["revision"] = json!("e".repeat(40));
    b["source"]["fingerprint"] = json!("f".repeat(40));
    b["source"]["dirty"] = json!(true);
    let (_, md) = compare(&a, &b).unwrap();
    // Every side states its complete identity: a baseline line without its fingerprint is not enough.
    assert!(md.contains(&format!(
        "Baseline: revision {}; source fingerprint {}; definitions {}; dirty false.\n\n",
        "b".repeat(40),
        "c".repeat(40),
        "a".repeat(40)
    )));
    assert!(md.contains(&format!(
        "Current: revision {}; source fingerprint {}; definitions {}; dirty true.\n\n",
        "e".repeat(40),
        "f".repeat(40),
        "a".repeat(40)
    )));
    assert!(md.contains("Shared machine class: fixture-cpu"));
    assert!(md.contains("Dirty source warning"));
    assert!(md.contains("&lt;script&gt;&#91;cpu&#93;&#40;https&#58;&#47;&#47;invalid&#41; &#96;code&#96; &#124; &amp; &#42;em&#42; &#35; heading"));
    assert!(!md.contains("<script>"));
    assert!(!md.contains("[cpu]"));
    assert!(!md.contains("flags_fingerprint"));
    assert!(!md.contains("/private/not-rendered"));
    assert!(!md.contains("https://"));
    // The warning is keyed on either side, not only on the current measurement.
    let mut dirty_base = fixture();
    dirty_base["source"]["dirty"] = json!(true);
    assert!(compare(&dirty_base, &fixture()).unwrap().1.contains("Dirty source warning"));
    assert!(!compare(&fixture(), &fixture()).unwrap().1.contains("Dirty source warning"));
    // A separator that is not a control scalar still must not split a summary line.
    a["environment"]["cpu_model"] = json!("line\u{2028}break\u{2029}end");
    let (_, separators) = compare(&a, &a).unwrap();
    assert!(!separators.contains('\u{2028}') && !separators.contains('\u{2029}'));
    assert!(separators.contains("line break end"));
    // The bound is bytes, not characters: 2048 two-byte scalars fit, 2049 do not.
    a["environment"]["cpu_model"] = json!("é".repeat(2048));
    assert!(compare(&a, &a).is_ok());
    a["environment"]["cpu_model"] = json!("é".repeat(2049));
    assert!(compare(&a, &a).is_err());
    // The header stays bounded by its own escape limit, whatever the table below it grows into.
    a["environment"]["cpu_model"] = json!("<".repeat(4096));
    let (_, md) = compare(&a, &a).unwrap();
    assert!(md.contains("\n| Workload |"));
    assert!(md.split("\n| Workload |").next().unwrap().len() < 30_000);
    a["environment"]["cpu_model"] = json!("<".repeat(4097));
    assert!(compare(&a, &a).is_err());
}

#[test]
fn an_unavailable_cpu_model_never_compares_even_against_an_identical_environment() {
    let mut same = fixture();
    same["environment"]["cpu_model"] = json!("unavailable");
    assert!(validate(&same).is_ok());
    assert_eq!(compare(&same, &same).unwrap_err(), "incomparable: CPU model unavailable");
}

use super::*;

fn jsonl_item(lines: &[&str], url: Option<&str>) -> String {
    serde_json::json!({
        "url": url,
        "lines": lines
            .iter()
            .map(|text| vec![serde_json::json!({ "text": text })])
            .collect::<Vec<_>>(),
    })
    .to_string()
}

fn repeated(item: &str, count: usize) -> String {
    vec![item; count].join("\n")
}

#[test]
fn parse_output_reads_items_and_skips_invalid_lines() {
    let output = format!(
        "{}\nnot json\n\n{}\n",
        jsonl_item(&["one"], Some("https://a.test")),
        r#"{"url":"https://b.test","lines":[[{"text":"two","color":"red","bold":true},{"text":"x\u0007y","color":"nope"}]]}"#,
    );
    let items = parse_output(output.as_bytes()).expect("valid items");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].url.as_deref(), Some("https://a.test"));
    assert_eq!(
        items[1].lines[0],
        vec![
            Span {
                text: "two".into(),
                color: Some(SpanColor::Red),
                bold: true,
            },
            Span {
                text: "xy".into(),
                color: None,
                bold: false,
            },
        ]
    );
}

#[test]
fn parse_output_treats_empty_output_as_empty_and_garbage_as_failure() {
    assert_eq!(parse_output(b"").expect("empty output"), Vec::new());
    assert_eq!(parse_output(b"\n  \n").expect("blank output"), Vec::new());
    assert!(parse_output(b"oops\n").is_err());
    assert!(parse_output(br#"{"lines":[]}"#).is_err());
}

#[test]
fn parse_output_drops_unsafe_urls_and_caps_lines() {
    let output = r#"{"url":"file:///etc/passwd","lines":[[{"text":"a"}],[{"text":"b"}],[{"text":"c"}],[{"text":"d"}]]}"#;
    let items = parse_output(output.as_bytes()).expect("valid item");
    assert_eq!(items[0].url, None);
    assert_eq!(items[0].lines.len(), MAX_LINES_PER_ITEM);
}

#[test]
fn plan_drops_overflow_from_the_last_section_first() {
    let mrs = repeated(&jsonl_item(&["a", "b"], None), 3);
    let artifacts = repeated(&jsonl_item(&["x"], None), 3);
    let sections = ForkSections::from_jsonl_for_test(&[("mrs", &mrs), ("artifacts", &artifacts)]);

    assert_eq!(planned(&sections, 13), vec![(0, 8, 3), (1, 5, 3)]);
    assert_eq!(planned(&sections, 11), vec![(0, 8, 3), (1, 3, 1)]);
    assert_eq!(planned(&sections, 10), vec![(0, 8, 3)]);
    assert_eq!(planned(&sections, 5), vec![(0, 4, 1)]);
    assert_eq!(planned(&sections, 0), Vec::new());
}

fn planned(sections: &ForkSections, budget: u16) -> Vec<(usize, u16, usize)> {
    sections
        .plan(budget)
        .into_iter()
        .map(|planned| (planned.index, planned.height, planned.max_items))
        .collect()
}

#[test]
fn plan_respects_max_items_and_hides_empty_sections() {
    let many = repeated(&jsonl_item(&["x"], None), 8);
    let sections = ForkSections::from_jsonl_for_test(&[("empty", ""), ("many", &many)]);
    assert_eq!(
        planned(&sections, 40),
        vec![(1, 2 + default_max_items() as u16, default_max_items())]
    );
}

#[test]
fn plan_uses_dragged_heights_within_the_budget() {
    let one = jsonl_item(&["x"], None);
    let mut sections = ForkSections::from_jsonl_for_test(&[("mrs", &one), ("artifacts", &one)]);

    sections.heights.insert("mrs".into(), 9);
    assert_eq!(planned(&sections, 20), vec![(0, 9, usize::MAX), (1, 3, 1)]);

    sections.heights.insert("mrs".into(), 50);
    assert_eq!(planned(&sections, 20), vec![(0, 20, usize::MAX)]);

    sections.heights.insert("mrs".into(), 1);
    assert_eq!(
        planned(&sections, 20),
        vec![(0, MIN_SECTION_ROWS, usize::MAX), (1, 3, 1)]
    );
}

fn placed_hits(placed: &[(usize, u16, u16)]) -> ForkSectionHits {
    ForkSectionHits {
        placed: placed
            .iter()
            .map(|(index, top, height)| Placed {
                index: *index,
                top: *top,
                height: *height,
            })
            .collect(),
        ..ForkSectionHits::default()
    }
}

#[test]
fn first_line_trades_rows_with_agents_and_keeps_spaces_room() {
    let one = jsonl_item(&["x"], None);
    let mut sections = ForkSections::from_jsonl_for_test(&[("mrs", &one), ("artifacts", &one)]);
    let hits = placed_hits(&[(0, 20, 8), (1, 28, 5)]);

    assert!(sections.resize_from_divider(0, 17, &hits));
    assert_eq!(sections.heights.get("mrs"), Some(&11));
    assert_eq!(sections.heights.get("artifacts"), None);

    assert!(sections.resize_from_divider(0, 2, &hits));
    assert_eq!(sections.heights.get("mrs"), Some(&(28 - MIN_SPLIT_ROWS)));

    assert!(!sections.resize_from_divider(0, 2, &hits));
    assert!(!sections.resize_from_divider(7, 2, &hits));
}

#[test]
fn later_lines_trade_rows_with_the_section_above() {
    let one = jsonl_item(&["x"], None);
    let mut sections = ForkSections::from_jsonl_for_test(&[("mrs", &one), ("artifacts", &one)]);
    let hits = placed_hits(&[(0, 20, 8), (1, 28, 5)]);

    assert!(sections.resize_from_divider(1, 30, &hits));
    assert_eq!(sections.heights.get("mrs"), Some(&10));
    assert_eq!(sections.heights.get("artifacts"), Some(&MIN_SECTION_ROWS));

    assert!(sections.resize_from_divider(1, 21, &hits));
    assert_eq!(sections.heights.get("mrs"), Some(&MIN_SECTION_ROWS));
    assert_eq!(sections.heights.get("artifacts"), Some(&10));
}

#[test]
fn dragged_heights_round_trip_through_the_state_file() {
    let path = std::env::temp_dir().join(format!(
        "hurdur-sidebar-heights-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    ));
    assert!(read_heights(&path).is_empty());

    let mut sections = ForkSections::from_jsonl_for_test(&[("mrs", "")]);
    sections.heights_path = Some(path.clone());
    sections.heights.insert("mrs".into(), 7);
    sections.save_heights();
    assert_eq!(read_heights(&path), sections.heights);

    std::fs::write(&path, "not json").expect("write heights");
    assert!(read_heights(&path).is_empty());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn apply_configs_keeps_rows_for_unchanged_sections() {
    let one = jsonl_item(&["x"], None);
    let mut sections = ForkSections::from_jsonl_for_test(&[("mrs", &one), ("artifacts", &one)]);
    let generation = sections.generation;

    sections.apply_configs(vec![
        test_section_config("mrs"),
        test_section_config("artifacts"),
    ]);
    assert_eq!(sections.generation, generation);

    let mut slower = test_section_config("mrs");
    slower.interval_seconds = 120;
    sections.apply_configs(vec![
        test_section_config("artifacts"),
        slower,
        test_section_config("new"),
    ]);
    assert_ne!(sections.generation, generation);
    assert_eq!(
        sections
            .sections
            .iter()
            .map(|section| section.items.len())
            .collect::<Vec<_>>(),
        vec![1, 1, 0]
    );
}

#[test]
fn tick_applies_current_results_and_keeps_rows_on_failure() {
    let mut sections = ForkSections::from_jsonl_for_test(&[("mrs", &jsonl_item(&["old"], None))]);
    let send = |sections: &ForkSections, generation, output| {
        sections
            .results_tx
            .send(RunResult {
                generation,
                index: 0,
                output,
            })
            .expect("receiver alive");
    };
    let now = Instant::now();

    send(&sections, sections.generation, Err("exited with 1".into()));
    assert!(!sections.tick(now));
    assert_eq!(sections.sections[0].items.len(), 1);

    send(&sections, sections.generation, Ok(b"garbage".to_vec()));
    assert!(!sections.tick(now));
    assert_eq!(sections.sections[0].items.len(), 1);

    send(
        &sections,
        sections.generation.wrapping_sub(1),
        Ok(Vec::new()),
    );
    assert!(!sections.tick(now));
    assert_eq!(sections.sections[0].items.len(), 1);

    send(&sections, sections.generation, Ok(Vec::new()));
    assert!(sections.tick(now));
    assert!(sections.sections[0].items.is_empty());
}

#[test]
fn read_configs_applies_defaults_and_treats_missing_file_as_empty() {
    let path = std::env::temp_dir().join(format!(
        "hurdur-sidebar-sections-{}-{}.toml",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    ));
    assert_eq!(read_configs(&path).expect("missing file"), Vec::new());

    std::fs::write(
        &path,
        "[[sections]]\ntitle = \"mrs\"\ncommand = \"mrs.sh\"\ninterval_seconds = 1\n",
    )
    .expect("write config");
    let configs = read_configs(&path).expect("valid config");
    assert_eq!(configs.len(), 1);
    assert_eq!(configs[0].max_items, default_max_items());
    assert_eq!(
        configs[0].interval(),
        Duration::from_secs(MIN_INTERVAL_SECONDS)
    );

    std::fs::write(&path, "[[sections]]\ntitle = \"mrs\"\n").expect("write config");
    assert!(read_configs(&path).is_err());
    let _ = std::fs::remove_file(&path);
}

#[cfg(unix)]
#[tokio::test]
async fn run_command_reports_output_exit_status_and_timeout() {
    assert_eq!(
        run_command("printf 'a\\nb'", Duration::from_secs(10))
            .await
            .expect("output"),
        b"a\nb"
    );
    assert!(run_command("exit 3", Duration::from_secs(10))
        .await
        .expect_err("non-zero exit")
        .contains("exited"));
    assert!(run_command("sleep 5", Duration::from_secs(1))
        .await
        .expect_err("timeout")
        .contains("timed out"));
}

#[cfg(unix)]
#[tokio::test]
async fn tick_runs_due_commands_and_applies_their_output() {
    let mut sections = ForkSections::empty(None);
    let mut config = test_section_config("mrs");
    config.command = format!(
        "printf '%s\\n' '{}'",
        jsonl_item(&["fresh"], Some("https://x.test"))
    );
    sections.apply_configs(vec![config]);

    assert!(!sections.tick(Instant::now()));
    assert!(sections.sections[0].running.is_some());
    let deadline = Instant::now() + Duration::from_secs(20);
    while !sections.tick(Instant::now()) {
        assert!(Instant::now() < deadline, "command result never arrived");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(sections.sections[0].running.is_none());
    assert_eq!(
        sections.sections[0].items[0].url.as_deref(),
        Some("https://x.test")
    );
}

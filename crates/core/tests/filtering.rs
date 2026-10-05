use futures_lite::future::block_on;
use xonata_core::filter::{DrawOptions, DrawSpan, FilterBounds, FilterHit, FilterQuery};
use xonata_core::storage::{NativeBacking, PageStore};
use xonata_core::{Engine, Event, Request, SearchQuery, SuggestionField};

fn fixture() -> Vec<u8> {
    let mut log = String::from("Kanata\t0004\nC=\t100\n");
    let ops = [
        (
            "vfirst.m x1",
            0,
            "Free RQU entries=36",
            vec![(100, 120), (121, 123)],
        ),
        ("vfirst.m x2", 0, "Free RQU entries=360", vec![(101, 122)]),
        ("vmsne.vv x3", 1, "target", vec![(110, 113)]),
        ("vmsne.vv x4", 0, "target", vec![(110, 115), (125, 128)]),
        ("vmsne.vv x5", 0, "target", vec![(120, 130)]),
        ("vmsne.vv x6", 0, "target", vec![(130, 132)]),
    ];
    let mut events = std::collections::BTreeMap::<u64, Vec<String>>::new();
    for (id, (name, thread, meta, stages)) in ops.iter().enumerate() {
        log.push_str(&format!(
            "I\t{id}\t{id}\t{thread}\nL\t{id}\t0\t{name}\nL\t{id}\t1\t{meta}\\nExtra=7\n"
        ));
        for &(start, end) in stages {
            events
                .entry(start)
                .or_default()
                .push(format!("S\t{id}\t0\tE\nL\t{id}\t2\tphase note\n"));
            events
                .entry(end)
                .or_default()
                .push(format!("E\t{id}\t0\tE\n"));
        }
    }
    for (cycle, records) in events {
        log.push_str(&format!("C=\t{cycle}\n"));
        for record in records {
            log.push_str(&record);
        }
    }
    for id in 0..ops.len() {
        log.push_str(&format!("R\t{id}\t{id}\t0\n"));
    }
    log.into_bytes()
}

#[test]
fn elapsed_sort_should_order_all_pages_with_stable_ids_and_leave_drawings_intact() {
    block_on(async {
        let mut bytes = String::from("Kanata\t0004\n");
        for id in 0..2400 {
            bytes.push_str(&format!(
                "I\t{id}\t{id}\t0\nS\t{id}\t0\tE\nC\t{}\nR\t{id}\t{id}\t0\n",
                id % 11 + 1
            ));
        }
        let mut engine = loaded(bytes.as_bytes()).await;
        assert_eq!(run(&mut engine, 1, "phase=E", 0).await, 2400);
        engine
            .request(Request::FilterSort {
                trace: 1,
                generation: 1,
                revision: 1,
                descending: true,
            })
            .await
            .unwrap();
        assert!(
            engine
                .tick()
                .await
                .unwrap()
                .iter()
                .any(|e| matches!(e, Event::FilterSortProgress { done: false, .. }))
        );
        // Replace an unfinished sort. Old pages must not be returned.
        engine
            .request(Request::FilterSort {
                trace: 1,
                generation: 1,
                revision: 2,
                descending: false,
            })
            .await
            .unwrap();
        assert!(
            engine
                .request(Request::FilterSort {
                    trace: 1,
                    generation: 1,
                    revision: 1,
                    descending: true
                })
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            engine
                .request(Request::SortedFilterResults {
                    trace: 1,
                    generation: 1,
                    revision: 1,
                    start: 0,
                    count: 128
                })
                .await
                .unwrap()
                .is_none()
        );
        while engine.has_search_work() {
            engine.tick().await.unwrap();
        }
        for (revision, descending) in [(2, false), (3, true)] {
            if revision == 3 {
                engine
                    .request(Request::FilterSort {
                        trace: 1,
                        generation: 1,
                        revision,
                        descending,
                    })
                    .await
                    .unwrap();
                while engine.has_search_work() {
                    engine.tick().await.unwrap();
                }
            }
            let mut expected: Vec<u64> = (0..2400).collect();
            expected.sort_unstable_by(|a, b| {
                let order = (a % 11).cmp(&(b % 11));
                (if descending { order.reverse() } else { order }).then_with(|| a.cmp(b))
            });
            let mut found = Vec::new();
            for start in (0..2400).step_by(128) {
                let Some(Event::SortedFilterResults { hits, .. }) = engine
                    .request(Request::SortedFilterResults {
                        trace: 1,
                        generation: 1,
                        revision,
                        start,
                        count: 999,
                    })
                    .await
                    .unwrap()
                else {
                    panic!("missing sorted page");
                };
                assert!(hits.len() <= 128);
                found.extend(hits.into_iter().map(|h| h.index));
            }
            assert_eq!(found, expected);
            assert!(
                matches!(engine.request(Request::RevealFilter {trace:1,generation:1,index:found[0]}).await.unwrap(),Some(Event::FilterSelection {hit,..}) if hit.source.op==found[0])
            );
        }
        assert_eq!(hits(&mut engine, 1, 0).await[0].index, 0);
        engine
            .request(Request::DrawFilter {
                trace: 1,
                generation: 1,
                label: String::new(),
            })
            .await
            .unwrap();
        engine
            .request(Request::FilterSort {
                trace: 1,
                generation: 1,
                revision: 4,
                descending: false,
            })
            .await
            .unwrap();
        // Drawing membership must remain independent even while sorting.
        engine
            .request(Request::FilterViewport {
                trace: 1,
                generation: 1,
                request: 1,
                bounds: FilterBounds {
                    cycle_start: 0,
                    cycle_end: u64::MAX,
                    row_start: 0,
                    row_end: u64::MAX,
                },
                selected: Some("2399".into()),
            })
            .await
            .unwrap();
        let mut drawn = None;
        while engine.has_search_work() {
            for e in engine.tick().await.unwrap() {
                if let Event::FilterDrawing { hits, visible, .. } = e {
                    drawn = Some((hits, visible));
                }
            }
        }
        let (drawn, visible) = drawn.unwrap();
        assert_eq!((drawn.len(), visible), (2048, 2400));
        assert!(drawn.iter().any(|h| h.index == 2399));
        engine
            .request(Request::FilterSort {
                trace: 1,
                generation: 1,
                revision: 5,
                descending: true,
            })
            .await
            .unwrap();
        engine
            .request(Request::CancelFilter {
                trace: 1,
                generation: 1,
            })
            .await
            .unwrap();
        assert!(!engine.has_search_work());
    });
}

#[test]
fn elapsed_sort_should_keep_signed_large_counts_exact_in_both_directions() {
    block_on(async {
        let mut engine = loaded(&fixture()).await;
        assert_eq!(
            run(
                &mut engine,
                1,
                "from=E & instr=vfirst -> to=E & instr=vmsne",
                0
            )
            .await,
            3
        );
        for (revision, descending) in [(1, false), (2, true)] {
            engine
                .request(Request::FilterSort {
                    trace: 1,
                    generation: 1,
                    revision,
                    descending,
                })
                .await
                .unwrap();
            while engine.has_search_work() {
                engine.tick().await.unwrap();
            }
            let Some(Event::SortedFilterResults { hits, .. }) = engine
                .request(Request::SortedFilterResults {
                    trace: 1,
                    generation: 1,
                    revision,
                    start: 0,
                    count: 128,
                })
                .await
                .unwrap()
            else {
                panic!("missing signed sort");
            };
            assert!(hits.windows(2).all(|w| if descending {
                w[0].elapsed >= w[1].elapsed
            } else {
                w[0].elapsed <= w[1].elapsed
            }));
            assert!(hits.iter().all(|h| h.elapsed < 0));
        }
        let origin = 1_u64 << 60;
        let bytes = format!(
            "Kanata\t0004\nI\t0\t0\t0\nI\t1\t1\t0\nS\t0\t0\tE\nS\t1\t0\tE\nC=\t{}\nE\t1\t0\tE\nC=\t{}\nE\t0\t0\tE\nR\t0\t0\t0\nR\t1\t1\t0\n",
            origin + 1,
            origin + 2
        );
        let mut engine = loaded(bytes.as_bytes()).await;
        run(&mut engine, 1, "phase=E", 0).await;
        engine
            .request(Request::FilterSort {
                trace: 1,
                generation: 1,
                revision: 1,
                descending: false,
            })
            .await
            .unwrap();
        while engine.has_search_work() {
            engine.tick().await.unwrap();
        }
        let Some(Event::SortedFilterResults { hits, .. }) = engine
            .request(Request::SortedFilterResults {
                trace: 1,
                generation: 1,
                revision: 1,
                start: 0,
                count: 128,
            })
            .await
            .unwrap()
        else {
            panic!("missing exact sort");
        };
        assert_eq!(
            hits.iter()
                .map(|h| (h.index, h.elapsed))
                .collect::<Vec<_>>(),
            vec![(1, i128::from(origin + 1)), (0, i128::from(origin + 2))]
        );
    });
}
fn exclude_query(base: &str, endpoint: &str, scope: &str) -> String {
    format!(
        "{base}; exclude={}; exclude_scope={scope}",
        serde_json::to_string(endpoint).unwrap()
    )
}
fn exclusion_fixture() -> Vec<u8> {
    let records = [
        ("source", 0, "E", 100, 110, ""),
        (
            "barrier",
            7,
            "X",
            115,
            125,
            "Prefix FREE RQU resource=36 suffix",
        ),
        ("left boundary", 0, "X", 110, 111, ""),
        ("right boundary", 0, "X", 120, 121, ""),
        ("target", 0, "E", 120, 123, ""),
        ("source", 0, "E", 200, 210, ""),
        ("outside time", 0, "X", 205, 207, ""),
        ("other phase", 0, "Y", 215, 216, ""),
        ("left boundary", 0, "X", 210, 212, ""),
        ("target", 0, "E", 220, 223, ""),
        ("target", 0, "E", 140, 145, ""),
        ("outside rows", 9, "X", 215, 216, ""),
    ];
    let mut text = String::from("Kanata\t0004\n");
    let mut events = std::collections::BTreeMap::<u64, Vec<String>>::new();
    for (id, (name, thread, phase, start, end, metadata)) in records.into_iter().enumerate() {
        text.push_str(&format!(
            "I\t{id}\t{id}\t{thread}\nL\t{id}\t0\t{name}\nL\t{id}\t1\t{metadata}\n"
        ));
        events.entry(start).or_default().push(format!(
            "S\t{id}\t1\t{phase}\nL\t{id}\t2\tEvent: buffer busy\n"
        ));
        events
            .entry(end)
            .or_default()
            .push(format!("E\t{id}\t1\t{phase}\n"));
    }
    for (cycle, records) in events {
        text.push_str(&format!("C=\t{cycle}\n"));
        for record in records {
            text.push_str(&record);
        }
    }
    for id in 0..12 {
        text.push_str(&format!("R\t{id}\t{id}\t0\n"));
    }
    text.into_bytes()
}
#[test]
fn exclusion_should_check_only_strict_interiors_after_pairing_across_threads() {
    block_on(async {
        let mut engine = loaded(&exclusion_fixture()).await;
        let base = "from=E & instr=source -> to=E & instr=target";
        assert_eq!(run(&mut engine, 1, base, 0).await, 2);
        // Reuses contains/wildcards, lane/status, numeric and scoped metadata fields.
        let query = exclude_query(
            base,
            "phase=X & instr=BARR* & meta_contains=\"rqu *=36\" & phase_meta_contains=\"buffer busy\" & lane=1 & status=retired & thread=7 & duration>=10 & cycle=115",
            "rows",
        );
        assert_eq!(run(&mut engine, 2, &query, 0).await, 1);
        let found = hits(&mut engine, 2, 0).await;
        assert_eq!(
            (
                found[0].index,
                found[0].source.op,
                found[0].target.as_ref().unwrap().op,
                found[0].elapsed
            ),
            (0, 5, 9, 10)
        );
        // The rejected source must not pair with later target 9 or 10.
        assert!(!found.iter().any(|h| h.source.op == 0));
        let query = exclude_query(base, "phase=X", "rows");
        assert_eq!(run(&mut engine, 3, &query, 0).await, 1);
        assert_eq!(hits(&mut engine, 3, 0).await[0].source.op, 5);
    });
}
#[test]
fn exclusion_scope_should_distinguish_intervening_rows_from_any_pipeline_row() {
    block_on(async {
        let mut engine = loaded(&exclusion_fixture()).await;
        let base = "from=E & instr=source -> to=E & instr=target";
        assert_eq!(
            run(
                &mut engine,
                1,
                &exclude_query(base, "phase=X & instr=\"outside rows\"", "rows"),
                0
            )
            .await,
            2
        );
        assert_eq!(
            run(
                &mut engine,
                2,
                &exclude_query(base, "phase=X & instr=\"outside rows\"", "all"),
                0
            )
            .await,
            1
        );
        assert_eq!(hits(&mut engine, 2, 0).await[0].source.op, 0);
    });
}
#[test]
fn exclusion_should_respect_selected_boundary_and_zero_or_negative_intervals() {
    block_on(async {
        let mut engine = loaded(&exclusion_fixture()).await;
        let base = "from=E & op=0 -> to=E & op=4";
        assert_eq!(
            run(
                &mut engine,
                1,
                &exclude_query(base, "phase=X & instr=barrier & phase_pos=START", "rows"),
                0
            )
            .await,
            0
        );
        assert_eq!(
            run(
                &mut engine,
                2,
                &exclude_query(base, "phase=X & instr=barrier & phase_pos=END", "rows"),
                0
            )
            .await,
            1
        );
        // Exclusion uses the ordered cycle bounds even for an overlapping pair.
        let base = "from=E & op=0 -> to=E & op=4";
        assert_eq!(
            run(
                &mut engine,
                3,
                &exclude_query(base, "phase=X & instr=\"left boundary\"", "rows"),
                0
            )
            .await,
            1
        );
        let bytes=b"Kanata\t0004\nI\t0\t0\t0\nI\t1\t1\t0\nI\t2\t2\t0\nC=\t100\nS\t0\t0\tE\nC=\t110\nS\t2\t0\tE\nC=\t115\nS\t1\t0\tX\nC=\t120\nE\t0\t0\tE\nE\t1\t0\tX\nE\t2\t0\tE\n";
        let mut engine = loaded(bytes).await;
        assert_eq!(
            run(&mut engine, 1, "from=E & op=0 -> to=E & op=2", 0).await,
            1
        );
        assert_eq!(
            run(
                &mut engine,
                2,
                &exclude_query("from=E & op=0 -> to=E & op=2", "phase=X", "rows"),
                0
            )
            .await,
            0
        );
        assert_eq!(
            run(
                &mut engine,
                3,
                &exclude_query(
                    "from=E & op=0 -> to=E & op=2 & phase_pos=END",
                    "phase=*",
                    "all"
                ),
                0
            )
            .await,
            1
        );
    });
}
#[test]
fn exclusion_should_reject_invalid_scope_and_single_phase_queries() {
    for query in [
        "phase=E; exclude=\"phase=X\"",
        "from=E -> to=E; exclude_scope=all",
        "from=E -> to=E; exclude=\"phase=X\"; exclude_scope=bad",
        "from=E -> to=E; exclude=\"instr=bad\"",
        "from=E -> to=E; exclude=\"phase=X\"; exclude=\"phase=Y\"",
    ] {
        assert!(FilterQuery::parse(query).is_err(), "accepted {query}");
    }
}
async fn loaded(bytes: &[u8]) -> Engine {
    let mut engine = Engine::new(Box::new(NativeBacking::new().unwrap()));
    engine.open(1, "filters.kanata".into()).await.unwrap();
    for part in bytes.chunks(4096) {
        engine.feed(1, part).await.unwrap();
    }
    engine.finish(1).await.unwrap();
    engine
}
async fn run(engine: &mut Engine, generation: u32, query: &str, skip: u64) -> u64 {
    let response = engine
        .request(Request::Filter {
            trace: 1,
            generation,
            text: query.into(),
            skip,
        })
        .await
        .unwrap();
    assert!(response.is_none(), "{response:?}");
    let mut total = 0;
    while engine.has_search_work() {
        for event in engine.tick().await.unwrap() {
            match event {
                Event::FilterProgress { total: found, .. } => total = found,
                Event::FilterError { message, .. } => panic!("{message}"),
                _ => {}
            }
        }
    }
    total
}
async fn hits(engine: &mut Engine, generation: u32, start: u64) -> Vec<FilterHit> {
    let Some(Event::FilterResults { hits, .. }) = engine
        .request(Request::FilterResults {
            trace: 1,
            generation,
            start,
            count: 128,
        })
        .await
        .unwrap()
    else {
        panic!("missing result page")
    };
    hits
}

#[test]
fn interval_pairing_should_use_later_same_thread_instructions_and_signed_gaps() {
    block_on(async {
        let mut engine = loaded(&fixture()).await;
        let query = "from=E & instr=vfirst & meta=\"FREE RQU *=36\" -> to=E & instr=vmsne";
        assert_eq!(run(&mut engine, 1, query, 0).await, 2);
        let found = hits(&mut engine, 1, 0).await;
        assert_eq!(
            found
                .iter()
                .map(|h| (h.source.op, h.target.as_ref().unwrap().op, h.elapsed))
                .collect::<Vec<_>>(),
            [(0, 3, -10), (0, 3, -13)]
        );
        assert_eq!(run(&mut engine, 2, query, 1).await, 2);
        assert_eq!(
            hits(&mut engine, 2, 0)
                .await
                .iter()
                .map(|h| h.elapsed)
                .collect::<Vec<_>>(),
            [0, -3]
        );
        assert_eq!(run(&mut engine, 3, query, 2).await, 2);
        assert_eq!(
            hits(&mut engine, 3, 0)
                .await
                .iter()
                .map(|h| h.elapsed)
                .collect::<Vec<_>>(),
            [10, 7]
        );
        assert_eq!(run(&mut engine, 4, query, 3).await, 0);
    });
}
#[test]
fn post_pair_constraints_should_not_substitute_another_target() {
    block_on(async {
        let mut engine = loaded(&fixture()).await;
        assert_eq!(
            run(
                &mut engine,
                1,
                "from=E & instr=vfirst -> to=E & instr=vmsne; gap>=0",
                0
            )
            .await,
            0
        );
        assert_eq!(
            run(
                &mut engine,
                2,
                "from=E & instr=vfirst -> to=E & instr=vmsne; gap>=0; row_distance<=4",
                1
            )
            .await,
            1
        );
    });
}
#[test]
fn endpoint_predicates_should_match_scoped_metadata_regex_and_numeric_fields() {
    block_on(async {
        let mut engine = loaded(&fixture()).await;
        let query = "phase=E & instr_re=\"vfirst\\.m\" & op_meta=\"Free RQU *=36\" & meta=Extra=7 & phase_meta=\"phase note\" & duration>=3 & global=0 & thread=0 & rid=0 & cycle>=121 & lane=0 & status=retired";
        // JSON string escaping requires an escaped backslash in the DSL.
        let query = query.replace("\\.m", "\\\\.m");
        assert_eq!(run(&mut engine, 1, &query, 0).await, 0);
        assert_eq!(
            run(
                &mut engine,
                2,
                &query.replace("duration>=3", "duration>=2"),
                0
            )
            .await,
            1
        );
        let found = hits(&mut engine, 2, 0).await;
        assert_eq!((found[0].source.stage, found[0].elapsed), (1, 2));
    });
}
#[test]
fn metadata_contains_should_match_partial_lines_with_wildcards_and_respect_scope() {
    block_on(async {
        let mut engine = loaded(&fixture()).await;
        assert_eq!(
            run(
                &mut engine,
                1,
                "phase=E & instr=vfirst & meta_contains=\"rqu *=36\"",
                0
            )
            .await,
            3
        );
        // Contains intentionally also accepts =360, unlike the legacy full-line matcher.
        assert_eq!(run(&mut engine, 2, "phase=E & instr=vfirst & op_meta_contains=\"RQU *36\" & phase_meta_contains=\"ase n\"", 0).await, 3);
        assert_eq!(
            run(
                &mut engine,
                3,
                "phase=E & instr=vfirst & phase_meta_contains=Extra",
                0
            )
            .await,
            0
        );
        assert_eq!(
            run(
                &mut engine,
                4,
                "phase=E & meta_contains=\"entries=*\\nExtra\"",
                0
            )
            .await,
            0
        );
    });
}

#[test]
fn suggestions_should_scan_in_bounded_batches_and_replace_stale_jobs() {
    block_on(async {
        let mut bytes = String::from("Kanata\t0004\n");
        for id in 0..900 {
            bytes.push_str(&format!("I\t{id}\t{id}\t0\nL\t{id}\t0\t{} x{id}\nS\t{id}\t0\tF\nL\t{id}\t2\tphase note\nL\t{id}\t1\tFree RQU entries={id}\nC\t1\nR\t{id}\t{id}\t0\n", if id < 850 { "add" } else { "vfirst" }));
        }
        let mut engine = loaded(bytes.as_bytes()).await;
        engine
            .request(Request::FilterSuggestions {
                trace: 1,
                generation: 1,
                field: SuggestionField::Instruction,
                text: "vfirst".into(),
            })
            .await
            .unwrap();
        let first = engine.tick().await.unwrap();
        assert!(first.iter().any(|e| matches!(e, Event::FilterSuggestions { generation: 1, values, done: false, .. } if values.is_empty())));
        let mut found = None;
        for _ in 0..10 {
            for event in engine.tick().await.unwrap() {
                if let Event::FilterSuggestions {
                    generation: 1,
                    values,
                    done: true,
                    ..
                } = event
                {
                    found = Some(values);
                }
            }
        }
        let values = found.unwrap();
        assert_eq!(values.len(), 16);
        assert!(values.iter().all(|v| v.starts_with("vfirst")));
        engine
            .request(Request::FilterSuggestions {
                trace: 1,
                generation: 2,
                field: SuggestionField::Metadata,
                text: "rqu *850".into(),
            })
            .await
            .unwrap();
        engine
            .request(Request::FilterSuggestions {
                trace: 1,
                generation: 3,
                field: SuggestionField::PhaseMetadata,
                text: "PHASE n".into(),
            })
            .await
            .unwrap();
        let mut phases = None;
        while engine.has_search_work() {
            for event in engine.tick().await.unwrap() {
                assert!(!matches!(
                    &event,
                    Event::FilterSuggestions { generation: 2, .. }
                ));
                if let Event::FilterSuggestions {
                    generation: 3,
                    values,
                    done: true,
                    ..
                } = event
                {
                    phases = Some(values);
                }
            }
        }
        assert_eq!(phases.unwrap(), ["phase note"]);
        engine
            .request(Request::FilterSuggestions {
                trace: 1,
                generation: 4,
                field: SuggestionField::OperationMetadata,
                text: "RQU *850".into(),
            })
            .await
            .unwrap();
        let mut metadata = None;
        while engine.has_search_work() {
            for event in engine.tick().await.unwrap() {
                if let Event::FilterSuggestions {
                    generation: 4,
                    values,
                    done: true,
                    ..
                } = event
                {
                    metadata = Some(values);
                }
            }
        }
        assert_eq!(metadata.unwrap(), ["Free RQU entries=850"]);
    });
}

#[test]
fn parser_should_report_invalid_fields_operators_regex_and_quotes_with_locations() {
    for query in [
        "phase=E & wat=1",
        "phase_pos=START",
        "phase=E & instr_re=\"[\"",
        "phase=E & meta=\"oops",
        "phase=E; row_pad=-1",
        "phase=E -> to=E -> to=F",
        "phase!=E",
        "phase=E; label=x; label=y",
    ] {
        let error = FilterQuery::parse(query).unwrap_err().to_string();
        assert!(error.contains("Filter at byte"), "{query}: {error}");
    }
    assert!(FilterQuery::parse("phase=E & meta=\"a & b -> c; d\"; label=\"a\\\"b\"").is_ok());
}
#[test]
fn large_cycle_values_and_open_phases_should_remain_exact_through_json() {
    block_on(async {
        let start = (1_u64 << 60) + 1;
        let bytes = format!(
            "Kanata\t0004\nC=\t{start}\nI\t0\t0\t0\nL\t0\t0\tx\nS\t0\t0\tE\nC\t3\nI\t1\t1\t0\nL\t1\t0\ty\nS\t1\t0\tE\nC\t2\nE\t0\t0\tE\n"
        );
        let mut engine = loaded(bytes.as_bytes()).await;
        assert_eq!(run(&mut engine, 1, "phase=E", 0).await, 2);
        let found = hits(&mut engine, 1, 0).await;
        assert_eq!(
            (
                found[0].source.start,
                found[0].elapsed,
                found[1].elapsed,
                found[1].source.open
            ),
            (start, 5, 3, true)
        );
        let value = serde_json::to_value(&found).unwrap();
        assert_eq!(value[0]["source"]["start"], start.to_string());
        assert_eq!(value[0]["elapsed"], "5");
        let decoded: Vec<FilterHit> = serde_json::from_value(value).unwrap();
        assert_eq!(decoded[0].source.start, start);
        assert_eq!(
            run(&mut engine, 2, "from=E & op=0 -> to=E & op=1", 0).await,
            1
        );
        assert_eq!(hits(&mut engine, 2, 0).await[0].elapsed, -2);
        engine
            .request(Request::Filter {
                trace: 1,
                generation: 3,
                text: "phase=E & phase_pos=END".into(),
                skip: 0,
            })
            .await
            .unwrap();
        let mut skipped = 0;
        while engine.has_search_work() {
            for e in engine.tick().await.unwrap() {
                if let Event::FilterProgress { skipped: n, .. } = e {
                    skipped = n;
                }
            }
        }
        assert_eq!(skipped, 1);
    });
}
#[test]
fn progressive_pages_draw_limit_and_generation_lifetimes_should_be_bounded() {
    block_on(async {
        let mut bytes = String::from("Kanata\t0004\n");
        for id in 0..2200 {
            bytes.push_str(&format!(
                "I\t{id}\t{id}\t0\nL\t{id}\t0\top{id}\nS\t{id}\t0\tE\nC\t1\nR\t{id}\t{id}\t0\n"
            ));
        }
        let mut engine = loaded(bytes.as_bytes()).await;
        engine
            .request(Request::Filter {
                trace: 1,
                generation: 1,
                text: "phase=E".into(),
                skip: 0,
            })
            .await
            .unwrap();
        let mut total = 0;
        while engine.has_search_work() {
            for e in engine.tick().await.unwrap() {
                if let Event::FilterProgress { total: n, .. } = e {
                    total = n;
                }
            }
            if total > 0 {
                let start = total.saturating_sub(7);
                let found = hits(&mut engine, 1, start).await;
                assert_eq!(found.len() as u64, total - start);
                assert_eq!(found[0].index, start);
            }
        }
        assert_eq!(total, 2200);
        let Some(Event::FilterDrawn { .. }) = engine
            .request(Request::DrawFilter {
                trace: 1,
                generation: 1,
                label: "all".into(),
            })
            .await
            .unwrap()
        else {
            panic!("not drawn")
        };
        assert_eq!(run(&mut engine, 2, "phase=E & op<2", 0).await, 2);
        assert!(
            engine
                .request(Request::FilterResults {
                    trace: 1,
                    generation: 1,
                    start: 0,
                    count: 128
                })
                .await
                .unwrap()
                .is_none()
        );
        let bounds = FilterBounds {
            cycle_start: 0,
            cycle_end: u64::MAX,
            row_start: 0,
            row_end: u64::MAX,
        };
        engine
            .request(Request::FilterViewport {
                trace: 1,
                generation: 1,
                request: 9,
                bounds,
                selected: Some("2199".into()),
            })
            .await
            .unwrap();
        let mut preview = None;
        while engine.has_search_work() {
            for e in engine.tick().await.unwrap() {
                if let Event::FilterDrawing {
                    hits,
                    visible,
                    request,
                    ..
                } = e
                {
                    preview = Some((hits, visible, request));
                }
            }
        }
        let (found, visible, request) = preview.unwrap();
        assert_eq!((found.len(), visible, request), (2048, 2200, 9));
        assert!(found.iter().any(|h| h.index == 2199));
        engine
            .request(Request::DrawFilter {
                trace: 1,
                generation: 2,
                label: String::new(),
            })
            .await
            .unwrap();
        engine
            .request(Request::FilterViewport {
                trace: 1,
                generation: 1,
                request: 10,
                bounds,
                selected: None,
            })
            .await
            .unwrap();
        assert!(!engine.has_search_work());
        engine
            .request(Request::CancelFilter {
                trace: 1,
                generation: 1,
            })
            .await
            .unwrap();
        assert_eq!(hits(&mut engine, 2, 0).await.len(), 2);
        engine
            .request(Request::CancelFilter {
                trace: 1,
                generation: 2,
            })
            .await
            .unwrap();
        engine
            .request(Request::FilterViewport {
                trace: 1,
                generation: 2,
                request: 11,
                bounds,
                selected: None,
            })
            .await
            .unwrap();
        while engine.has_search_work() {
            for e in engine.tick().await.unwrap() {
                if let Event::FilterDrawing { visible, .. } = e {
                    assert_eq!(visible, 2);
                }
            }
        }
        engine
            .request(Request::ClearFilterDrawing { trace: 1 })
            .await
            .unwrap();
        engine.close(1).await.unwrap();
    });
}
#[test]
fn storage_cleanup_should_separate_filter_generations_search_and_trace_pages() {
    block_on(async {
        let mut store = PageStore::new(Box::new(NativeBacking::new().unwrap()), 1024);
        store
            .write_filter(1, 1, "results-0", &vec![1_u64])
            .await
            .unwrap();
        store
            .write_filter(1, 2, "results-0", &vec![2_u64])
            .await
            .unwrap();
        store.write_hits(1, 1, 0, &[(0, 42)]).await.unwrap();
        store.clear_filter(1, 1).await.unwrap();
        assert!(
            store
                .read_filter::<Vec<u64>>(1, 1, "results-0")
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store
                .read_filter::<Vec<u64>>(1, 2, "results-0")
                .await
                .unwrap(),
            [2]
        );
        assert_eq!(store.read_hits(1, 1, 0).await.unwrap(), [(0, 42)]);
        store.clear_search(1).await.unwrap();
        assert_eq!(
            store
                .read_filter::<Vec<u64>>(1, 2, "results-0")
                .await
                .unwrap(),
            [2]
        );
    });
}
#[test]
fn bounds_should_cover_both_rows_negative_gaps_and_padding() {
    block_on(async {
        let mut engine = loaded(&fixture()).await;
        run(&mut engine, 1, "from=E & op=0 -> to=E & op=3", 0).await;
        let hit = &hits(&mut engine, 1, 0).await[0];
        let b = hit.bounds(
            &DrawOptions {
                span: DrawSpan::Both,
                row_pad: 2,
                label: String::new(),
            },
            6,
        );
        assert_eq!(
            (b.cycle_start, b.cycle_end, b.row_start, b.row_end),
            (110, 120, 0, 5)
        );
        assert_eq!(
            hit.bounds(
                &DrawOptions {
                    span: DrawSpan::Target,
                    ..Default::default()
                },
                6
            )
            .row_start,
            3
        );
    });
}
#[test]
fn ordinary_search_should_not_delete_drawn_filter_data() {
    block_on(async {
        let mut engine = loaded(&fixture()).await;
        run(&mut engine, 1, "phase=E", 0).await;
        engine
            .request(Request::DrawFilter {
                trace: 1,
                generation: 1,
                label: String::new(),
            })
            .await
            .unwrap();
        engine
            .request(Request::Search {
                trace: 1,
                generation: 1,
                query: Box::new(SearchQuery::default()),
            })
            .await
            .unwrap();
        while engine.has_search_work() {
            engine.tick().await.unwrap();
        }
        assert_eq!(hits(&mut engine, 1, 0).await.len(), 8);
    });
}

#[test]
fn target_indexes_should_survive_thread_cache_eviction() {
    block_on(async {
        let mut bytes = String::from("Kanata\t0004\n");
        for id in 0..300 {
            let name = if id < 100 { "source" } else { "target" };
            bytes.push_str(&format!(
                "I\t{id}\t{id}\t{}\nL\t{id}\t0\t{name}\nS\t{id}\t0\tE\nC\t1\nR\t{id}\t{id}\t0\n",
                id % 100
            ));
        }
        let mut engine = loaded(bytes.as_bytes()).await;
        assert_eq!(
            run(
                &mut engine,
                1,
                "from=E & instr=source -> to=E & instr=target",
                1
            )
            .await,
            100
        );
        let found = hits(&mut engine, 1, 0).await;
        for hit in found {
            assert_eq!(hit.target.as_ref().unwrap().op, hit.source.op + 200);
            assert_eq!(hit.elapsed, 199);
        }
    });
}

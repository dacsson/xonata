use async_trait::async_trait;
use futures_lite::future::block_on;
use serde_json::Value;
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};
use xonata_core::{
    AnalysisAction as A, AnalysisRequest, BackingStore, Engine, Event, Request, SearchQuery,
    StoreError,
};

type Objects = BTreeMap<(u32, String), Vec<u8>>;

#[derive(Clone, Default)]
struct Memory(Rc<RefCell<Objects>>);

#[async_trait(?Send)]
impl BackingStore for Memory {
    async fn read(&mut self, t: u32, k: &str) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self.0.borrow().get(&(t, k.into())).cloned())
    }

    async fn write(&mut self, t: u32, k: &str, v: &[u8]) -> Result<(), StoreError> {
        self.0.borrow_mut().insert((t, k.into()), v.to_vec());
        Ok(())
    }

    async fn remove(&mut self, t: u32) -> Result<(), StoreError> {
        self.0.borrow_mut().retain(|(id, _), _| *id != t);
        Ok(())
    }

    async fn clear_prefix(&mut self, t: u32, p: &str) -> Result<(), StoreError> {
        self.0
            .borrow_mut()
            .retain(|(id, k), _| *id != t || !k.starts_with(p));
        Ok(())
    }

    async fn clear_search(&mut self, t: u32) -> Result<(), StoreError> {
        self.clear_prefix(t, "search-").await
    }

    async fn clear_filter(&mut self, t: u32, g: u32) -> Result<(), StoreError> {
        self.clear_prefix(t, &format!("filter-{g}-")).await
    }
}

async fn call(e: &mut Engine, session: u32, action: A) -> Value {
    e.analysis_request(AnalysisRequest {
        trace: 1,
        session,
        request: 1,
        action,
    })
    .await
    .unwrap()
}

async fn loaded(count: usize) -> (Engine, Memory) {
    let memory = Memory::default();
    let mut engine = Engine::new(Box::new(memory.clone()));
    engine.open(1, "analysis.kanata".into()).await.unwrap();
    engine
        .feed(1, b"Kanata\t0004\nC=\t9007199254740993\n")
        .await
        .unwrap();
    for id in 0..count {
        let text = format!(
            "I\t{id}\t{id}\t0\nL\t{id}\t0\t{}\nS\t{id}\t0\tE\nC\t1\nE\t{id}\t0\tE\nR\t{id}\t{id}\t0\n",
            if id % 2 == 0 { "alpha" } else { "beta" }
        );
        engine.feed(1, text.as_bytes()).await.unwrap();
    }
    engine.finish(1).await.unwrap();
    (engine, memory)
}

#[test]
fn independent_queries_preserve_viewer_search_filter_and_drawing() {
    block_on(async {
        let (mut engine, memory) = loaded(5000).await;
        engine
            .request(Request::Search {
                trace: 1,
                generation: 1,
                query: Box::new(SearchQuery {
                    text: "alpha".into(),
                    ..Default::default()
                }),
            })
            .await
            .unwrap();
        engine
            .request(Request::Filter {
                trace: 1,
                generation: 1,
                text: "phase=E & instr=alpha".into(),
                skip: 0,
            })
            .await
            .unwrap();
        while engine.has_search_work() {
            engine.tick().await.unwrap();
        }
        engine
            .request(Request::DrawFilter {
                trace: 1,
                generation: 1,
                label: "viewer".into(),
            })
            .await
            .unwrap();
        call(
            &mut engine,
            10,
            A::Search {
                query: Box::new(SearchQuery {
                    text: "beta".into(),
                    ..Default::default()
                }),
            },
        )
        .await;
        call(
            &mut engine,
            11,
            A::Search {
                query: Box::new(SearchQuery {
                    text: "alpha".into(),
                    ..Default::default()
                }),
            },
        )
        .await;
        call(
            &mut engine,
            10,
            A::Filter {
                text: "phase=E & instr=beta".into(),
            },
        )
        .await;
        call(
            &mut engine,
            11,
            A::Filter {
                text: "phase=E & instr=alpha".into(),
            },
        )
        .await;
        assert_eq!(
            call(
                &mut engine,
                10,
                A::SearchResults {
                    start: 0,
                    count: 20
                }
            )
            .await["hits"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
        let mut saw_partial = false;
        for _ in 0..1000 {
            if !engine.has_search_work() {
                break;
            }
            engine.tick().await.unwrap();
            let status = call(&mut engine, 10, A::Status).await;
            if status["filter_done"] == false {
                saw_partial = true;
            }
        }
        assert!(saw_partial);
        assert!(!engine.has_search_work());
        for (session, label) in [(10, "beta"), (11, "alpha")] {
            let status = call(&mut engine, session, A::Status).await;
            assert_eq!(status["search_total"], "2500");
            assert_eq!(status["filter_total"], "2500");
            let page = call(
                &mut engine,
                session,
                A::SearchResults {
                    start: 0,
                    count: 1000,
                },
            )
            .await;
            assert_eq!(page["hits"].as_array().unwrap().len(), 64);
            assert_eq!(page["hits"][0]["op"]["label"], label);
            call(&mut engine, session, A::Sort { descending: true }).await;
        }
        while engine.has_search_work() {
            engine.tick().await.unwrap();
        }
        let before = call(
            &mut engine,
            10,
            A::FilterResults {
                start: 0,
                count: 64,
                sorted: false,
            },
        )
        .await;
        let sorted = call(
            &mut engine,
            10,
            A::FilterResults {
                start: 0,
                count: 64,
                sorted: true,
            },
        )
        .await;
        assert_eq!(before["hits"][0]["index"], sorted["hits"][0]["index"]);
        assert!(
            before["hits"][0]["source"]["cycle"]
                .as_str()
                .unwrap()
                .parse::<u64>()
                .unwrap()
                > 9_007_199_254_740_992
        );
        call(&mut engine, 10, A::Cancel).await;
        assert!(
            !memory
                .0
                .borrow()
                .keys()
                .any(|(_, k)| k.starts_with("analysis-10-"))
        );
        assert!(
            memory
                .0
                .borrow()
                .keys()
                .any(|(_, k)| k.starts_with("analysis-11-"))
        );
        let Some(Event::Results { hits, .. }) = engine
            .request(Request::Results {
                trace: 1,
                generation: 1,
                start: 0,
                count: 1,
            })
            .await
            .unwrap()
        else {
            panic!("viewer search lost")
        };
        assert_eq!(hits[0].op.label, "alpha");
        let Some(Event::FilterResults { hits, .. }) = engine
            .request(Request::FilterResults {
                trace: 1,
                generation: 1,
                start: 0,
                count: 1,
            })
            .await
            .unwrap()
        else {
            panic!("viewer filter lost")
        };
        assert_eq!(hits[0].source.instruction, "alpha");
        // Drawing data remains after analysis session cleanup.
        assert!(
            memory
                .0
                .borrow()
                .keys()
                .any(|(_, k)| k.starts_with("filter-1-"))
        );
        engine.close(1).await.unwrap();
        assert!(memory.0.borrow().is_empty());
        assert!(
            engine
                .analysis_request(AnalysisRequest {
                    trace: 1,
                    session: 11,
                    request: 2,
                    action: A::Status
                })
                .await
                .is_err()
        );
    });
}

#[test]
fn failures_restore_viewer_context_and_cancellation_is_idempotent() {
    block_on(async {
        let (mut engine, _) = loaded(4).await;
        assert!(
            engine
                .analysis_request(AnalysisRequest {
                    trace: 1,
                    session: 5,
                    request: 1,
                    action: A::Filter {
                        text: "phase=E & invalid=1".into()
                    }
                })
                .await
                .is_err()
        );
        assert!(
            engine
                .analysis_request(AnalysisRequest {
                    trace: 1,
                    session: 5,
                    request: 2,
                    action: A::Operation { id: 999 }
                })
                .await
                .is_err()
        );
        assert_eq!(
            call(&mut engine, 5, A::Rows { start: 0, count: 1 }).await["rows"][0]["op"]["label"],
            "alpha"
        );
        assert_eq!(call(&mut engine, 6, A::Info).await["complete"], true);
        assert!(
            engine
                .analysis_request(AnalysisRequest {
                    trace: 1,
                    session: 7,
                    request: 1,
                    action: A::Info
                })
                .await
                .is_err()
        );
        call(&mut engine, 5, A::Cancel).await;
        call(&mut engine, 5, A::Cancel).await;
        call(&mut engine, 7, A::Info).await;
        let Some(Event::View { rows, .. }) = engine
            .request(Request::View {
                trace: 1,
                generation: 4,
                start: 0,
                stride: 1,
                count: 2,
                hide_flushed: false,
            })
            .await
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(rows.len(), 2);
        let mut loading = Engine::new(Box::new(Memory::default()));
        loading.open(1, "loading".into()).await.unwrap();
        assert!(
            loading
                .analysis_request(AnalysisRequest {
                    trace: 1,
                    session: 1,
                    request: 1,
                    action: A::Info
                })
                .await
                .is_err()
        );
    });
}

#[test]
fn analysis_phase_pairing_preserves_signed_gaps_and_strict_exclusions() {
    block_on(async {
        let mut engine = Engine::new(Box::new(Memory::default()));
        engine.open(1, "pairs.kanata".into()).await.unwrap();
        engine.feed(1,b"Kanata\t0004\nI\t0\t0\t0\nL\t0\t0\tsource\nI\t1\t1\t7\nL\t1\t0\tbarrier\nI\t2\t2\t0\nL\t2\t0\ttarget\nI\t3\t3\t0\nL\t3\t0\ttarget\nC=\t100\nS\t0\t0\tE\nC=\t105\nS\t2\t0\tE\nC=\t110\nE\t0\t0\tE\nE\t2\t0\tE\nC=\t115\nS\t1\t0\tX\nC=\t116\nE\t1\t0\tX\nC=\t120\nS\t3\t0\tE\nC=\t125\nE\t3\t0\tE\nR\t0\t0\t0\nR\t1\t1\t0\nR\t2\t2\t0\nR\t3\t3\t0\n").await.unwrap();
        engine.finish(1).await.unwrap();
        call(
            &mut engine,
            1,
            A::Filter {
                text: "from=E & instr=source -> to=E & instr=target; gap<0".into(),
            },
        )
        .await;
        call(
            &mut engine,
            2,
            A::Filter {
                text:
                    "from=E & instr=source -> to=E & op=3; exclude=\"phase=X\"; exclude_scope=all"
                        .into(),
            },
        )
        .await;
        while engine.has_search_work() {
            engine.tick().await.unwrap();
        }
        let page = call(
            &mut engine,
            1,
            A::FilterResults {
                start: 0,
                count: 20,
                sorted: false,
            },
        )
        .await;
        assert_eq!(page["hits"][0]["elapsed"], "-5");
        assert_eq!(page["hits"][0]["target"]["op"], "2");
        assert_eq!(call(&mut engine, 2, A::Status).await["filter_total"], "0");
        let detail = call(&mut engine, 1, A::Operation { id: 0 }).await;
        assert_eq!(detail["operation"]["stages"][0]["phase"], "E");
        call(
            &mut engine,
            1,
            A::Filter {
                text: "from=E & instr=source -> to=E & instr=target; gap>=0".into(),
            },
        )
        .await;
        while engine.has_search_work() {
            engine.tick().await.unwrap();
        }
        assert_eq!(call(&mut engine, 1, A::Status).await["filter_total"], "0");
    });
}

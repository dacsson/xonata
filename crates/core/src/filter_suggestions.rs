//! Bounded trace-backed autocomplete, independent of filter and search jobs.
use crate::filter::expression;
use crate::storage::{PageStore, StoreError};
use crate::{Event, SuggestionField, TraceInfo, symbol};
use regex::Regex;
use std::collections::BTreeSet;

pub(crate) struct Suggestions {
    generation: u32,
    field: SuggestionField,
    matcher: Regex,
    pages: Vec<u64>,
    cursor: usize,
    values: BTreeSet<String>,
}
impl Suggestions {
    pub fn new(
        generation: u32,
        field: SuggestionField,
        text: &str,
        pages: Vec<u64>,
    ) -> Result<Self, StoreError> {
        if text.len() > 2048 {
            return Err(StoreError::Input("Suggestion pattern is too long".into()));
        }
        Ok(Self {
            generation,
            field,
            matcher: expression(text, false, true, 0)?,
            pages,
            cursor: 0,
            values: BTreeSet::new(),
        })
    }
    fn add(&mut self, value: &str) {
        if self.values.len() < 16 && !value.is_empty() && self.matcher.is_match(value) {
            // A truncated value would silently change the user's constraint when selected.
            if value.chars().take(161).count() <= 160 {
                self.values.insert(value.to_owned());
            }
        }
    }
    pub async fn tick(
        &mut self,
        store: &mut PageStore,
        info: &TraceInfo,
    ) -> Result<Event, StoreError> {
        if let Some(page) = self.pages.get(self.cursor) {
            for id in store.page_ids(info.id, *page) {
                if let Some(op) = store.get(info.id, id).await? {
                    if self.field == SuggestionField::Instruction {
                        self.add(op.label.lines().next().unwrap_or(""));
                    }
                    if matches!(
                        self.field,
                        SuggestionField::Metadata | SuggestionField::OperationMetadata
                    ) {
                        for line in op.detail.lines() {
                            self.add(line);
                        }
                    }
                    for stage in &op.stages {
                        match self.field {
                            SuggestionField::Phase => self.add(symbol(&info.symbols, stage.name)),
                            SuggestionField::Lane => self.add(symbol(&info.symbols, stage.lane)),
                            SuggestionField::Metadata | SuggestionField::PhaseMetadata => {
                                for line in stage.labels.lines() {
                                    self.add(line);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                if self.values.len() == 16 {
                    break;
                }
            }
            self.cursor += 1;
        }
        Ok(Event::FilterSuggestions {
            trace: info.id,
            generation: self.generation,
            field: self.field,
            values: self.values.iter().cloned().collect(),
            done: self.cursor >= self.pages.len() || self.values.len() == 16,
        })
    }
}

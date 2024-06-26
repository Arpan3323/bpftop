/**
 *
 *  Copyright 2024 Netflix, Inc.
 *
 *  Licensed under the Apache License, Version 2.0 (the "License");
 *  you may not use this file except in compliance with the License.
 *  You may obtain a copy of the License at
 *
 *  http://www.apache.org/licenses/LICENSE-2.0
 *
 *  Unless required by applicable law or agreed to in writing, software
 *  distributed under the License is distributed on an "AS IS" BASIS,
 *  WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 *  See the License for the specific language governing permissions and
 *  limitations under the License.
 *
 */
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use circular_buffer::CircularBuffer;
use libbpf_rs::query::ProgInfoIter;
use ratatui::widgets::TableState;
use tui_input::Input;

use crate::bpf_program::BpfProgram;

pub struct App {
    pub mode: Mode,
    pub state: Arc<Mutex<TableState>>,
    pub items: Arc<Mutex<Vec<BpfProgram>>>,
    pub data_buf: Arc<Mutex<CircularBuffer<20, PeriodMeasure>>>,
    pub max_cpu: f64,
    pub max_eps: i64,
    pub max_runtime: u64,
    pub filter_input: Arc<Mutex<Input>>,
}

pub struct PeriodMeasure {
    pub cpu_time_percent: f64,
    pub events_per_sec: i64,
    pub average_runtime_ns: u64,
}

#[derive(Debug, PartialEq)]
pub enum Mode {
    Table,
    Graph,
    Filter,
}

impl App {
    pub fn new() -> App {
        App {
            mode: Mode::Table,
            state: Arc::new(Mutex::new(TableState::default())),
            items: Arc::new(Mutex::new(vec![])),
            data_buf: Arc::new(Mutex::new(CircularBuffer::<20, PeriodMeasure>::new())),
            max_cpu: 0.0,
            max_eps: 0,
            max_runtime: 0,
            filter_input: Arc::new(Mutex::new(Input::default())),
        }
    }

    pub fn start_background_thread(&self) {
        let items = Arc::clone(&self.items);
        let data_buf = Arc::clone(&self.data_buf);
        let state = Arc::clone(&self.state);
        let filter = Arc::clone(&self.filter_input);

        thread::spawn(move || loop {
            let loop_start = Instant::now();

            let mut items = items.lock().unwrap();
            let map: HashMap<String, BpfProgram> = items
                .drain(..)
                .map(|prog| (prog.id.clone(), prog))
                .collect();

            let filter = filter.lock().unwrap();
            let filter_str = filter.value().to_lowercase();
            drop(filter);

            let iter = ProgInfoIter::default();
            for prog in iter {
                let instant = Instant::now();

                let prog_name = match prog.name.to_str() {
                    Ok(name) => name.to_string(),
                    Err(_) => continue,
                };

                if prog_name.is_empty() {
                    continue;
                }

                // Skip bpf program if it does not match filter
                let bpf_type = prog.ty.to_string();
                if !filter_str.is_empty()
                    && !bpf_type.to_lowercase().contains(&filter_str)
                    && !prog_name.to_lowercase().contains(&filter_str)
                {
                    continue;
                }

                let mut bpf_program = BpfProgram {
                    id: prog.id.to_string(),
                    bpf_type,
                    name: prog_name,
                    prev_runtime_ns: 0,
                    run_time_ns: prog.run_time_ns,
                    prev_run_cnt: 0,
                    run_cnt: prog.run_cnt,
                    instant,
                    period_ns: 0,
                };

                if let Some(prev_bpf_program) = map.get(&bpf_program.id) {
                    bpf_program.prev_runtime_ns = prev_bpf_program.run_time_ns;
                    bpf_program.prev_run_cnt = prev_bpf_program.run_cnt;
                    bpf_program.period_ns = prev_bpf_program.instant.elapsed().as_nanos();
                }

                items.push(bpf_program);
            }

            let mut state = state.lock().unwrap();
            let mut data_buf = data_buf.lock().unwrap();
            if let Some(index) = state.selected() {
                // If the selected index is out of bounds, unselect it.
                // This can happen if a program exits while it's selected.
                if index >= items.len() {
                    state.select(None);
                    continue;
                }

                let bpf_program = &items[index];
                data_buf.push_back(PeriodMeasure {
                    cpu_time_percent: bpf_program.cpu_time_percent(),
                    events_per_sec: bpf_program.events_per_second(),
                    average_runtime_ns: bpf_program.period_average_runtime_ns(),
                });
            }

            // Explicitly drop the MutexGuards to unlock before sleeping.
            drop(items);
            drop(data_buf);
            drop(state);

            // Adjust sleep duration to maintain a 1-second sample period, accounting for loop processing time.
            let elapsed = loop_start.elapsed();
            let sleep = if elapsed > Duration::from_secs(1) {
                Duration::from_secs(1)
            } else {
                Duration::from_secs(1) - elapsed
            };
            thread::sleep(sleep);
        });
    }

    pub fn toggle_graphs(&mut self) {
        self.data_buf.lock().unwrap().clear();
        self.max_cpu = 0.0;
        self.max_eps = 0;
        self.max_runtime = 0;
        self.mode = match &self.mode {
            Mode::Table => Mode::Graph,
            _ => Mode::Table,
        }
    }

    pub fn selected_program(&self) -> Option<BpfProgram> {
        let items = self.items.lock().unwrap();
        let state = self.state.lock().unwrap();

        state.selected().map(|i| items[i].clone())
    }

    pub fn next_program(&mut self) {
        let items = self.items.lock().unwrap();
        if items.len() > 0 {
            let mut state = self.state.lock().unwrap();
            let i = match state.selected() {
                Some(i) => {
                    if i >= items.len() - 1 {
                        0
                    } else {
                        i + 1
                    }
                }
                None => 0,
            };
            state.select(Some(i));
        }
    }

    pub fn previous_program(&mut self) {
        let items = self.items.lock().unwrap();
        if items.len() > 0 {
            let mut state = self.state.lock().unwrap();
            let i = match state.selected() {
                Some(i) => {
                    if i == 0 {
                        items.len() - 1
                    } else {
                        i - 1
                    }
                }
                None => items.len() - 1,
            };
            state.select(Some(i));
        }
    }

    pub fn toggle_filter(&mut self) {
        self.mode = match &self.mode {
            Mode::Table => Mode::Filter,
            _ => Mode::Table,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_next_program_with_empty() {
        let mut app = App::new();

        // Initially no item is selected
        assert_eq!(app.selected_program(), None);

        // After calling next, no item should be selected
        app.next_program();
        assert_eq!(app.selected_program(), None);
    }

    #[test]
    fn test_next_program() {
        let mut app = App::new();
        let prog_1 = BpfProgram {
            id: "1".to_string(),
            bpf_type: "test".to_string(),
            name: "test".to_string(),
            prev_runtime_ns: 100,
            run_time_ns: 200,
            prev_run_cnt: 1,
            run_cnt: 2,
            instant: Instant::now(),
            period_ns: 0,
        };

        let prog_2 = BpfProgram {
            id: "2".to_string(),
            bpf_type: "test".to_string(),
            name: "test".to_string(),
            prev_runtime_ns: 100,
            run_time_ns: 200,
            prev_run_cnt: 1,
            run_cnt: 2,
            instant: Instant::now(),
            period_ns: 0,
        };

        // Add some dummy BpfPrograms to the items vector
        app.items.lock().unwrap().push(prog_1.clone());
        app.items.lock().unwrap().push(prog_2.clone());

        // Initially no item is selected
        assert_eq!(app.selected_program(), None);

        // After calling next, the first item should be selected
        app.next_program();
        assert_eq!(app.selected_program(), Some(prog_1.clone()));

        // After calling next again, the second item should be selected
        app.next_program();
        assert_eq!(app.selected_program(), Some(prog_2.clone()));

        // After calling next again, we should wrap around to the first item
        app.next_program();
        assert_eq!(app.selected_program(), Some(prog_1.clone()));
    }

    #[test]
    fn test_previous_program_with_empty() {
        let mut app = App::new();

        // Initially no item is selected
        assert_eq!(app.selected_program(), None);

        // After calling previous, no item should be selected
        app.previous_program();
        assert_eq!(app.selected_program(), None);
    }

    #[test]
    fn test_previous_program() {
        let mut app = App::new();
        let prog_1 = BpfProgram {
            id: "1".to_string(),
            bpf_type: "test".to_string(),
            name: "test".to_string(),
            prev_runtime_ns: 100,
            run_time_ns: 200,
            prev_run_cnt: 1,
            run_cnt: 2,
            instant: Instant::now(),
            period_ns: 0,
        };

        let prog_2 = BpfProgram {
            id: "2".to_string(),
            bpf_type: "test".to_string(),
            name: "test".to_string(),
            prev_runtime_ns: 100,
            run_time_ns: 200,
            prev_run_cnt: 1,
            run_cnt: 2,
            instant: Instant::now(),
            period_ns: 0,
        };

        // Add some dummy BpfPrograms to the items vector
        app.items.lock().unwrap().push(prog_1.clone());
        app.items.lock().unwrap().push(prog_2.clone());

        // Initially no item is selected
        assert_eq!(app.selected_program(), None);

        // After calling previous, the last item should be selected
        app.previous_program();
        assert_eq!(app.selected_program(), Some(prog_2.clone()));

        // After calling previous again, the first item should be selected
        app.previous_program();
        assert_eq!(app.selected_program(), Some(prog_1.clone()));

        // After calling previous again, we should wrap around to the last item
        app.previous_program();
        assert_eq!(app.selected_program(), Some(prog_2.clone()));
    }

    #[test]
    fn test_toggle_graphs() {
        let mut app = App::new();

        // Initially, UI should be in table mode
        assert_eq!(app.mode, Mode::Table);

        // After calling toggle_graphs, UI should be in graph mode
        app.toggle_graphs();
        assert_eq!(app.mode, Mode::Graph);

        // Set max_cpu, max_eps, and max_runtime to non-zero values
        app.max_cpu = 10.0;
        app.max_eps = 5;
        app.max_runtime = 100;
        app.data_buf.lock().unwrap().push_back(PeriodMeasure {
            cpu_time_percent: 10.0,
            events_per_sec: 5,
            average_runtime_ns: 100,
        });

        // After calling toggle_graphs, UI should be in table mode again
        app.toggle_graphs();
        assert_eq!(app.mode, Mode::Table);

        // max_cpu, max_eps, and max_runtime should be reset to 0
        assert_eq!(app.max_cpu, 0.0);
        assert_eq!(app.max_eps, 0);
        assert_eq!(app.max_runtime, 0);

        // and data_buf should be empty again
        assert!(app.data_buf.lock().unwrap().is_empty());
    }
}

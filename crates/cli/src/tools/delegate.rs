//! `delegate_task`: jalankan sub-agent paralel, atau simpan rencana dulu.
//!
//! Dua mode:
//! - **inline**: `{ tasks: […] }` → dijalankan langsung (perilaku lama).
//! - **plan**: `{ tasks: […], plan_id: "x" }` → rencana **disimpan** ke
//!   `.minic/plans/x.json` dan **tidak** dijalankan. Untuk mengeksekusi:
//!   `{ plan_id: "x", run: true }`. Ini memberi manusia titik review sebelum
//!   token dibakar.

use crate::plans;
use crate::subagent::SubAgentPool;
use futures_core::future::BoxFuture;
use minic_core::error::AgentError;
use minic_core::tool::{Tool, ToolContext};
use minic_core::types::ToolResult;
use serde_json::{Value, json};
use std::sync::Arc;

pub struct DelegateTask {
    pool: Arc<SubAgentPool>,
}

impl DelegateTask {
    pub fn new(pool: Arc<SubAgentPool>) -> Self {
        Self { pool }
    }
}

impl Tool for DelegateTask {
    fn name(&self) -> &str {
        "delegate_task"
    }

    fn description(&self) -> &str {
        "Delegate independent read-only investigation tasks to parallel sub-agents (up to the configured limit, default 10). Each returns a concise report. Sub-agents cannot modify files or delegate further. Supply plan_id to save the task list for review instead of running it; later call with { plan_id, run: true } to execute the saved plan."
    }

    fn is_mutating(&self) -> bool {
        true
    }

    fn parameters_schema(&self) -> Value {
        let max = self.pool.capacity();
        json!({
            "type": "object",
            "properties": {
                "tasks": {
                    "type": "array",
                    "items": { "type": "string" },
                    "minItems": 1,
                    "maxItems": max,
                    "description": "Independent investigation tasks; run in parallel. Omit when using plan_id."
                },
                "plan_id": {
                    "type": "string",
                    "description": "Letters, digits, '-' and '_' (max 64). With tasks: save the plan and do NOT run it. Alone: refer to a saved plan."
                },
                "run": {
                    "type": "boolean",
                    "default": false,
                    "description": "Only meaningful with plan_id: actually execute the saved plan."
                }
            },
            "required": []
        })
    }

    fn execute<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolResult, AgentError>> {
        Box::pin(async move {
            let plan_id: Option<String> = args
                .get("plan_id")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let run = args.get("run").and_then(|v| v.as_bool()).unwrap_or(false);

            // Mode rencana: ambil tugas dari rencana tersimpan.
            if let Some(id) = &plan_id {
                return self
                    .run_from_plan(&ctx.cwd, id, run, args.get("tasks"))
                    .await;
            }

            // Mode inline: tugas diberikan langsung.
            if run {
                return Ok(ToolResult::err(
                    String::new(),
                    "delegate_task",
                    "'run' is only meaningful with plan_id".to_string(),
                ));
            }
            let tasks = match self.parse_tasks(args.get("tasks")) {
                Ok(t) => t,
                Err(r) => return Ok(r),
            };
            let results = self.pool.run_many(tasks).await;
            let mut out = String::new();
            let mut failures = 0usize;
            for (index, result) in results.iter().enumerate() {
                match result {
                    Ok(text) => {
                        out.push_str(&format!(
                            "### sub-agent {} (ok)\n{}\n\n",
                            index + 1,
                            text.trim()
                        ));
                    }
                    Err(error) => {
                        failures += 1;
                        out.push_str(&format!(
                            "### sub-agent {} (failed)\n{error}\n\n",
                            index + 1
                        ));
                    }
                }
            }
            let mut tool_result =
                ToolResult::ok(String::new(), "delegate_task", out.trim_end().to_string());
            tool_result.is_error = failures == results.len();
            Ok(tool_result)
        })
    }
}

impl DelegateTask {
    /// Baca array `tasks` (atau string tunggal). `None`/bentuk salah → error.
    fn parse_tasks(&self, value: Option<&Value>) -> Result<Vec<String>, ToolResult> {
        let err = |msg: &str| ToolResult::err(String::new(), "delegate_task", msg.to_string());
        let tasks: Vec<String> = match value {
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect(),
            Some(Value::String(single)) => vec![single.clone()],
            Some(_) => {
                return Err(err(
                    "'tasks' must be an array of strings or a single string",
                ));
            }
            None => return Err(err("missing 'tasks' (or a plan_id)")),
        };
        if tasks.is_empty() {
            return Err(err("no tasks provided"));
        }
        let max = self.pool.capacity();
        if tasks.len() > max {
            return Err(err(&format!(
                "too many sub-agents: {} requested, maximum is {max}",
                tasks.len()
            )));
        }
        Ok(tasks)
    }

    /// Jalankan mode rencana: simpan (dengan tasks) atau eksekusi (dengan run).
    async fn run_from_plan(
        &self,
        cwd: &std::path::Path,
        id: &str,
        run: bool,
        tasks_value: Option<&Value>,
    ) -> Result<ToolResult, AgentError> {
        if let Err(reason) = plans::validate_id(id) {
            return Ok(ToolResult::err(String::new(), "delegate_task", reason));
        }

        // Simpan dulu bila ada tasks (mode save).
        if let Some(tasks_value) = tasks_value {
            let tasks = match self.parse_tasks(Some(tasks_value)) {
                Ok(t) => t,
                Err(r) => return Ok(r),
            };
            let plan = plans::draft(id, tasks);
            match plans::save(cwd, &plan) {
                Ok(path) => Ok(ToolResult::ok(
                    String::new(),
                    "delegate_task",
                    format!(
                        "plan saved (not executed): {} → {}\n\n{}",
                        id,
                        path.display(),
                        format_plan(&plan)
                    ),
                )),
                Err(e) => Ok(ToolResult::err(String::new(), "delegate_task", e)),
            }
        } else if run {
            // Eksekusi rencana tersimpan.
            let mut plan = match plans::load(cwd, id) {
                Ok(Some(p)) => p,
                Ok(None) => {
                    return Ok(ToolResult::err(
                        String::new(),
                        "delegate_task",
                        format!("no saved plan named {id:?}"),
                    ));
                }
                Err(e) => return Ok(ToolResult::err(String::new(), "delegate_task", e)),
            };
            let tasks = match self.parse_tasks(Some(&serde_json::json!(plan.tasks.clone()))) {
                Ok(t) => t,
                Err(r) => return Ok(r),
            };
            let results = self.pool.run_many(tasks).await;
            let mut out = String::new();
            let mut failures = 0usize;
            for (index, result) in results.iter().enumerate() {
                match result {
                    Ok(text) => out.push_str(&format!(
                        "### sub-agent {} (ok)\n{}\n\n",
                        index + 1,
                        text.trim()
                    )),
                    Err(error) => {
                        failures += 1;
                        out.push_str(&format!(
                            "### sub-agent {} (failed)\n{error}\n\n",
                            index + 1
                        ));
                    }
                }
            }
            if failures == results.len() {
                // Tidak ada yang berhasil: jangan tandai plan sebagai executed,
                // biar bisa dicoba lagi tanpa harus memaksa `run`.
                let mut r =
                    ToolResult::err(String::new(), "delegate_task", out.trim_end().to_string());
                r.name = "delegate_task".to_string();
                return Ok(r);
            }
            let _ = plans::mark_executed(cwd, &mut plan);
            let mut r = ToolResult::ok(String::new(), "delegate_task", out.trim_end().to_string());
            r.name = "delegate_task".to_string();
            Ok(r)
        } else {
            Ok(ToolResult::err(
                String::new(),
                "delegate_task",
                format!(
                    "plan {id:?} was saved without tasks; to run it call {{ \"plan_id\": \"{id}\", \"run\": true }}"
                ),
            ))
        }
    }
}

/// Ringkasan rencana untuk pesan tool (membantu model melihat apa yang akan
/// dijalankan tanpa membuka berkas).
fn format_plan(plan: &plans::Plan) -> String {
    let mut out = format!(
        "plan {} ({}) · {} task(s)\n",
        plan.id,
        plan.status.as_str(),
        plan.tasks.len()
    );
    for (i, task) in plan.tasks.iter().enumerate() {
        out.push_str(&format!("  {}. {task}\n", i + 1));
    }
    out.trim_end().to_string()
}

# Workflow Engine

How `nin-workflow` turns a `workflow` object into running jobs. For the user-facing syntax see the Reference (§8); this document explains the behaviour you can rely on.

## Model

* A **workflow** has inputs, environment, defaults and a set of **jobs**.
* A **job** has `needs`, an optional matrix, steps, `finally` steps, limits and outputs.
* A **step** is exactly one of `run` (process), `script` (NIN statements) or `uses` (function call).
* A matrix job expands into **instances** (`build[target=a]`). Scheduling works on instances.

## Planning

`plan()` validates the graph and returns execution *levels*: all jobs in a level can run in parallel once the previous levels are done. It reports unknown `needs`, self-dependencies and cycles (with the cycle path) as errors. `nin workflow list` prints the levels, `nin workflow graph` prints the DAG. The same checks run in `nin check` (E0602/E0603), so a broken graph never reaches the engine.

Selecting jobs (`--job build`) runs those jobs plus everything they transitively need.

## Scheduling

* Every instance whose dependencies have all *finished* is ready. Ready instances start on worker threads, limited by `max_parallel`: the `--max-parallel` option, else `workflow.max_parallel`, else the CPU count.
* A matrix may set its own `max_parallel` for its instances.
* A job that `needs` a matrix job waits for **all** of its instances. Its `jobs.<id>.result` is `success` only if every instance succeeded (`continue_on_error` instances count as success).

## Results and conditions

Each job ends as `success`, `failure`, `skipped` or `cancelled`.

* A job runs only if all `needs` succeeded, unless its `if` says otherwise. `if` can use `success()`, `failure()`, `always()`, `cancelled()` and the whole context (`jobs.*`, `inputs`, `matrix`, `env`).
* A job whose condition is false, or whose dependencies failed, is `skipped`.
* `continue_on_error: true` on a job or step records the failure in the report but does not fail dependents or the workflow.
* The **workflow outcome** is `failure` if any job that is not `continue_on_error` failed; otherwise `cancelled` if the user cancelled or any job was cancelled; otherwise `success` (skipped jobs do not count against it). The CLI exit code follows: 0 success, 1 failure, 130 cancelled.
* For a matrix job the instance results are aggregated: all `success` → `success`; any `failure` → `failure`; all `skipped` → `skipped`; anything else → `cancelled`.

## Steps

Steps run in order inside a job; a failing step stops the remaining normal steps unless `continue_on_error` is set. `finally` steps run afterwards even after a failure, a timeout or a cancellation.

* **`run`** builds a `ProcSpec`, starts the process inside the job's process group, captures output and exit code, and exposes `steps.<id>.{result,outputs,code,stdout,stderr}`.
* **`script`** runs statements in an isolated child scope with the step context; `output` statements fill `steps.<id>.outputs`.
* **`uses`** calls a declared function; `with` keys map to parameter names, missing required ones are a static error.
* `on_error` statements run when the step ends in failure; `error` is bound to the failure.

## Retries and timeouts

* `retries: n` re-runs a failing **step** or a failing **job** up to `n` more times (a job retry restarts all of its steps), waiting `retry_delay` between attempts; the workflow `defaults.retries` applies to jobs that set none. Each attempt is logged (`attempt 1 failed — retrying`) and the report records `attempts`.
* `timeout` covers one attempt. A watchdog thread sets a cancel token at the deadline; the running step observes it, kills its process tree and the attempt fails with a timeout message. A step's effective timeout never exceeds the time left in its job's deadline.
* Cancelled attempts are not retried; a cancellation also stops the retry loop of the job.

## Cancellation

Three independent sources share one token chain per instance: the user token (Ctrl+C, `Options.cancel`), the fail-fast token, and per-attempt timeout tokens.

* **Fail-fast**: when a job fails (and is not `continue_on_error`), the fail-fast token is set for the **whole workflow**: running jobs are cancelled and pending jobs are skipped. For a matrix with `strategy.fail_fast: false` the sibling instances are allowed to finish first; the token is set only after all instances are done and at least one failed.
* **Ctrl+C**: the CLI sets the user token; every running process tree is killed, `finally` steps run, the report shows `cancelled`, exit code 130.

## Resource limits and process control

`limits` (`memory`, `processes`, `cpu_time`) apply to every process a job starts.

* Windows: one Job Object per job group — `JOB_OBJECT_LIMIT_JOB_MEMORY`, `ACTIVE_PROCESS`, `JOB_TIME`, plus kill-on-close so nothing outlives the engine. Cancelling terminates the whole job object, i.e. the entire process tree.
* Linux: per-process `RLIMIT_AS`, `RLIMIT_NPROC`, `RLIMIT_CPU` set between fork and exec; processes get their own process group so cancel can signal the tree.

A process that exceeds a limit fails its step with a message that names the limit when the OS reports it.

## Output

`ConsoleSink` prints process output with a `[job/step]` prefix after each process finishes (output is buffered per process, not streamed live); `PrefixSink` adds a prefix for programmatic runs and tests. Output is written a chunk at a time under a lock, so chunks from parallel jobs are not split mid-chunk.

## Report

`--report file.json` writes `{name, outcome, duration_ms, jobs:[{id, result, duration_ms, attempts, outputs, steps:[{id, name, result, code, duration_ms, attempts, ...}]}]}`. A text summary table is always printed.

## Inputs

`inputs` are declared with a type and optional default; `--input k=v` supplies string values that are converted to the declared type (`int`, `float`, `bool`, `string`). Missing required inputs and values that fail conversion stop the run before any job starts.

## Dry run

`--dry-run` expands matrices and resolves inputs, then prints one `[dry-run] job <key> (after <needs>)` line per instance and reports every job and step as `skipped` with outcome `success`. No process or script runs.

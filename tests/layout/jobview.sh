#!/usr/bin/env bash
# Copyright © 2016-2026 The SENTIENT Authors
#
# Every long job shows in the one job view (app/src/manager/jobview.rs) and
# declares its steps before it starts. Fails when
#   - a job page draws a job's progress itself: its own step checklist
#     (mark_steps), the current step line (step_with_time), a progress bar
#     or a tail of the log;
#   - a job on a job page starts without its steps (Job::set_plan, or the
#     page's plan_run) in the lines before it, unless the start is marked
#     "jobview: <why>" (a short job shown elsewhere, or a plan that comes
#     from the agent);
#   - a long `--job` (the agent on a server, a root job) is missing from
#     crate::jobplan::for_job, so it would send no Progress::Plan.
#   tests/layout/jobview.sh
set -u
cd "$(dirname "$0")/../../app/src" || exit 2
bad=0
# The pages that run long jobs.
PAGES="manager/backup.rs manager/restore.rs manager/update.rs manager/movedata.rs manager/repair.rs
manager/migration.rs manager/remote_pages.rs manager/instances.rs installer/pages.rs installer/mod.rs
maintenance.rs earlier.rs manager/server_card.rs"
for f in $PAGES; do
  [ -f "$f" ] || { echo "missing job page: $f"; bad=1; continue; }
  # 1. Progress drawn outside the shared view.
  while IFS= read -r hit; do
    line=${hit%%:*}
    prev=$(sed -n "$((line > 1 ? line - 1 : 1)),${line}p" "$f")
    grep -q 'jobview:' <<<"$prev" && continue
    echo "progress outside the shared job view: $f:$hit"; bad=1
  done < <(grep -nE 'mark_steps\(|step_with_time\(\)|(ProgressBar|Progress)::new\("[a-z-]*(progress|maint)|\.log\(\)\.(last|iter\(\)\.rev\(\)\.take)\(' "$f")
  # 2. A job started without its steps.
  while IFS= read -r line; do
    from=$((line > 30 ? line - 30 : 1))
    if ! sed -n "${from},${line}p" "$f" | grep -qE 'set_plan\(|plan_run\(|jobview:'; then
      echo "a job starts without declaring its steps (set_plan): $f:$line"; bad=1
    fi
  done < <(grep -nE '\b(job|j)\.start\($|\b(job|j)\.start\(move|=> job\.start\(' "$f" | cut -d: -f1)
done
# 3. Every long --job sends its plan first.
for j in backup-now scheduled-backup backup update rollback repair uninstall migrate resume-migration move-data apply-changes take-over-compose docker-restart system-updates; do
  grep -qE "\"$j\"( \||\s*=>| =>)" jobplan.rs || { echo "--job $j declares no steps (crate::jobplan::for_job)"; bad=1; }
done
grep -q 'Progress::Plan { steps: crate::jobplan::restore' headless.rs || { echo "--job restore-run declares no steps"; bad=1; }
grep -q 'crate::jobplan::for_job' headless.rs || { echo "crate::headless does not send the plan of a --job"; bad=1; }
# 4. The view itself: a fixed log height, a bounded log, a fixed pill.
grep -q 'pub const LOG_HEIGHT: f32 = 280.;' manager/jobview.rs || { echo "the log card is not 280 px high"; bad=1; }
grep -q 'pub const LOG_KEEP: usize' jobs.rs || { echo "the kept log lines are not bounded"; bad=1; }
grep -q 'const PILL_W' manager/jobview.rs || { echo "the status pill has no fixed width"; bad=1; }
[ $bad -eq 0 ] && echo "jobview.sh: every long job uses the shared view and declares its steps"
exit $bad

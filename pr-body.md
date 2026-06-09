Allow toggling job enabled state without deleting and recreating.

**Changes:**
- `cron_add`: accept `enabled` field (default `true`) so callers can create jobs in a paused state
- `cron_update`: accept `enabled` field to toggle on/off without altering schedule/command
- `PATCH /api/cron/:id`: thread `enabled` through to `CronJobPatch`; make `agent` optional for non-command patches (e.g. pure enable/disable)
- Frontend Cron page: add Pause/Resume icon button per job; i18n keys for pause/resume
- Frontend API: expose `enabled` in `patchCronJob` type

Closes #7356

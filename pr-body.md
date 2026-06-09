**Issue**: The Scheduled Jobs edit modal was missing fields that are present in the "+ Add Job" interface. Users could not edit `agent`, `model`, `session_target`, `allowed_tools`, or `delivery` when editing an existing job.

**Changes:**

### Backend (`crates/zeroclaw-gateway/src/api.rs`)
- Extended `CronPatchBody` to accept `delivery`, `model`, `session_target`, and `allowed_tools` fields
- Updated `handle_api_cron_patch` to extract and forward these fields into `CronJobPatch`

### Frontend API (`web/src/lib/api.ts`)
- Extended `patchCronJob` patch type to include `agent`, `delivery`, `model`, `session_target`, and `allowed_tools`

### Frontend Modal (`web/src/pages/Cron.tsx`)
- Removed `{!isEditing && ...}` guards around Agent selector, Model input, Session Target buttons, Allowed Tools input, and Delivery config section — all now visible during edit
- Updated `handleSubmit` edit branch to build the full patch payload including all new fields
- Added logic to clear delivery config when switching from `announce` to `none` mode during edit

Closes #6891

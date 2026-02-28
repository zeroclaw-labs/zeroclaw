package ai.zeroclaw.android.worker

import android.content.Context
import androidx.work.*
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.util.concurrent.TimeUnit

/**
 * WorkManager worker that runs periodic heartbeat checks.
 *
 * This handles:
 * - Cron job execution
 * - Health checks
 * - Scheduled agent tasks
 *
 * Respects Android's Doze mode and battery optimization.
 */
class HeartbeatWorker(
    context: Context,
    params: WorkerParameters
) : CoroutineWorker(context, params) {

    override suspend fun doWork(): Result = withContext(Dispatchers.IO) {
        try {
            // Get task type from input data
            val taskType = inputData.getString(KEY_TASK_TYPE) ?: TASK_HEARTBEAT

            when (taskType) {
                TASK_HEARTBEAT -> runHeartbeat()
                TASK_CRON -> runCronJob()
                TASK_HEALTH_CHECK -> runHealthCheck()
                else -> runHeartbeat()
            }

            Result.success()
        } catch (e: Exception) {
            if (runAttemptCount < 3) {
                Result.retry()
            } else {
                Result.failure(workDataOf(KEY_ERROR to e.message))
            }
        }
    }

    private suspend fun runHeartbeat() {
        // TODO: Connect to ZeroClaw bridge
        // val bridge = ZeroClawBridge
        // bridge.sendHeartbeat()

        // For now, just log
        android.util.Log.d(TAG, "Heartbeat executed")
    }

    private suspend fun runCronJob() {
        val jobId = inputData.getString(KEY_JOB_ID)
        val prompt = inputData.getString(KEY_PROMPT)

        // TODO: Execute cron job via bridge
        // ZeroClawBridge.executeCronJob(jobId, prompt)

        android.util.Log.d(TAG, "Cron job executed: $jobId")
    }

    private suspend fun runHealthCheck() {
        // TODO: Check agent status
        // val status = ZeroClawBridge.getStatus()

        android.util.Log.d(TAG, "Health check executed")
    }

    companion object {
        private const val TAG = "HeartbeatWorker"

        const val KEY_TASK_TYPE = "task_type"
        const val KEY_JOB_ID = "job_id"
        const val KEY_PROMPT = "prompt"
        const val KEY_ERROR = "error"

        const val TASK_HEARTBEAT = "heartbeat"
        const val TASK_CRON = "cron"
        const val TASK_HEALTH_CHECK = "health_check"

        const val WORK_NAME_HEARTBEAT = "zeroclaw_heartbeat"

        /**
         * Schedule periodic heartbeat (every 15 minutes minimum for WorkManager)
         */
        fun scheduleHeartbeat(context: Context, intervalMinutes: Long = 15) {
            // WorkManager enforces 15-minute minimum for periodic work
            val effectiveInterval = maxOf(intervalMinutes, 15L)

            val constraints = Constraints.Builder()
                .setRequiredNetworkType(NetworkType.CONNECTED)
                .build()

            val request = PeriodicWorkRequestBuilder<HeartbeatWorker>(
                effectiveInterval, TimeUnit.MINUTES
            )
                .setConstraints(constraints)
                .setInputData(workDataOf(KEY_TASK_TYPE to TASK_HEARTBEAT))
                .setBackoffCriteria(BackoffPolicy.EXPONENTIAL, 1, TimeUnit.MINUTES)
                .build()

            // Use UPDATE policy to apply new interval settings immediately
            WorkManager.getInstance(context).enqueueUniquePeriodicWork(
                WORK_NAME_HEARTBEAT,
                ExistingPeriodicWorkPolicy.UPDATE,
                request
            )
        }

        /**
         * Schedule a one-time cron job
         */
        fun scheduleCronJob(
            context: Context,
            jobId: String,
            prompt: String,
            delayMs: Long
        ) {
            val request = OneTimeWorkRequestBuilder<HeartbeatWorker>()
                .setInputData(workDataOf(
                    KEY_TASK_TYPE to TASK_CRON,
                    KEY_JOB_ID to jobId,
                    KEY_PROMPT to prompt
                ))
                .setInitialDelay(delayMs, TimeUnit.MILLISECONDS)
                .build()

            WorkManager.getInstance(context).enqueue(request)
        }

        /**
         * Cancel heartbeat
         */
        fun cancelHeartbeat(context: Context) {
            WorkManager.getInstance(context).cancelUniqueWork(WORK_NAME_HEARTBEAT)
        }
    }
}

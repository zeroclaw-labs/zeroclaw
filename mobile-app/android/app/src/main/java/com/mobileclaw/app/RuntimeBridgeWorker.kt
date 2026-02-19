package com.mobileclaw.app

import android.content.Context
import androidx.work.Worker
import androidx.work.WorkerParameters

class RuntimeBridgeWorker(appContext: Context, params: WorkerParameters) : Worker(appContext, params) {
    override fun doWork(): Result {
        return try {
            val result = RuntimeBridge.runBackgroundTick(applicationContext)
            if (result.failed > 0 && result.delivered == 0) {
                Result.retry()
            } else {
                Result.success()
            }
        } catch (_: Throwable) {
            Result.retry()
        }
    }
}

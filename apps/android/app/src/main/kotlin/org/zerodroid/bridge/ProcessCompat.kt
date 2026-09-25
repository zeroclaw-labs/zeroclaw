package org.zerodroid.bridge

import java.util.concurrent.TimeUnit

/** Process helpers shared by the supervisor and its shutdown path. */
object ProcessCompat {

    fun isAlive(process: Process): Boolean = process.isAlive

    fun waitFor(process: Process, timeoutMs: Long): Boolean =
        process.waitFor(timeoutMs, TimeUnit.MILLISECONDS)

    fun destroyForcibly(process: Process) {
        process.destroyForcibly()
    }
}

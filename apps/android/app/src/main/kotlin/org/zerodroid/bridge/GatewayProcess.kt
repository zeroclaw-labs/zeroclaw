package org.zerodroid.bridge

import android.util.Log
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference

/**
 * Supervises the bundled rust zeroclaw gateway as a child process.
 *
 * Hardening (per the architecture + code reviews):
 *  - a monotonic GENERATION token guards against rapid stop()/start() spawning two supervisors:
 *    every supervisor loop and ready-probe carries the generation it was born in and no-ops once a
 *    newer generation exists. stop() bumps the generation and joins the old supervisor.
 *  - stdout+stderr are merged into one pipe and drained continuously (a full pipe would deadlock the
 *    child while it looks "running").
 *  - every exit is waitFor()'d (reaped); grandchildren are zeroclaw's own responsibility.
 *  - unexpected exits restart with capped exponential backoff; a clean stop() does not restart.
 *  - state callbacks are fired OUTSIDE the object lock.
 *  - the gateway obeys OS sleep (no wakelocks); START_STICKY recreation re-enters start().
 */
class GatewayProcess(
    private val rt: NativeRuntime,
    private val argv: () -> List<String>,
    private val onState: (State) -> Unit,
    private val onLogLine: (String) -> Unit,
    private val present: () -> Boolean = { rt.gatewayPresent },
    private val tag: String = "zc-gateway"
) {
    enum class State { STOPPED, STARTING, RUNNING, RESTARTING, FAILED }

    private val wantRunning = AtomicBoolean(false)
    private val generation = AtomicInteger(0)
    private val proc = AtomicReference<Process?>(null)
    @Volatile private var supervisor: Thread? = null
    @Volatile var state: State = State.STOPPED
        private set

    // Process.pid() isn't in every Android API-level stub; resolve reflectively so it compiles on
    // minSdk 31 and simply returns null where the runtime lacks it.
    val pid: Int? get() = proc.get()?.let {
        try { (it.javaClass.getMethod("pid").invoke(it) as Long).toInt() } catch (e: Throwable) { null }
    }

    fun start() {
        if (!present()) { log("FATAL: bundled binary missing for $tag"); setState(State.FAILED); return }
        val sup: Thread
        synchronized(this) {
            if (wantRunning.get()) return
            wantRunning.set(true)
            val gen = generation.incrementAndGet()
            sup = Thread({ supervise(gen) }, "zc-supervisor-$gen").apply { isDaemon = true }
            supervisor = sup
        }
        sup.start()
    }

    fun stop() {
        val old: Thread?
        val p: Process?
        synchronized(this) {
            wantRunning.set(false)
            generation.incrementAndGet()      // invalidate any in-flight supervisor/ready-probe
            old = supervisor; supervisor = null
            p = proc.getAndSet(null)
        }
        p?.let { killTree(it) }
        old?.interrupt()
        try { old?.join(4000) } catch (e: InterruptedException) {}
        setState(State.STOPPED)
    }

    private fun supervise(gen: Int) {
        var attempt = 0
        while (wantRunning.get() && gen == generation.get()) {
            setState(if (attempt == 0) State.STARTING else State.RESTARTING)
            val started = System.currentTimeMillis()
            val code = runOnce(gen)
            val ranMs = System.currentTimeMillis() - started
            if (!wantRunning.get() || gen != generation.get()) break
            if (ranMs > HEALTHY_MS) attempt = 0
            attempt++
            val backoff = minOf(BACKOFF_BASE_MS * (1L shl minOf(attempt, 6)), BACKOFF_CAP_MS)
            log("gateway exited (code=$code, ran=${ranMs}ms) — restart #$attempt in ${backoff}ms")
            setState(State.RESTARTING)
            try { Thread.sleep(backoff) } catch (e: InterruptedException) { break }
        }
        if (gen == generation.get() && !wantRunning.get()) setState(State.STOPPED)
    }

    /** One full lifecycle of the child; returns its exit code (or -1). Blocks until it exits. */
    private fun runOnce(gen: Int): Int {
        return try {
            val pb = ProcessBuilder(argv())
                .directory(rt.home).redirectErrorStream(true)  // merge stderr→stdout: one pipe, no deadlock
            rt.applyEnv(pb)
            val p = pb.start()
            if (gen != generation.get()) { killTree(p); return -1 }  // stopped while spawning
            proc.set(p)
            val drain = Thread({ drainStream(p) }, "zc-drain").apply { isDaemon = true }.also { it.start() }
            Thread({
                try { Thread.sleep(1500) } catch (e: InterruptedException) { return@Thread }
                if (ProcessCompat.isAlive(p) && wantRunning.get() && gen == generation.get()) {
                    setState(State.RUNNING)
                }
            }, "zc-ready").apply { isDaemon = true }.start()
            val code = p.waitFor()           // reap
            drain.join(2000)
            proc.compareAndSet(p, null)
            code
        } catch (e: Exception) {
            log("spawn error: ${e.message}")
            -1
        }
    }

    private fun drainStream(p: Process) {
        try {
            val writer = rotatingLogWriter()
            p.inputStream.bufferedReader().forEachLine { line ->
                onLogLine(line)
                writer?.let { try { it.write(line); it.write("\n"); it.flush() } catch (e: Exception) {} }
            }
            writer?.close()
        } catch (e: Exception) { /* stream closed on exit */ }
    }

    private fun rotatingLogWriter(): java.io.Writer? = try {
        if (rt.logFile.exists() && rt.logFile.length() > LOG_MAX_BYTES) rt.logFile.delete()
        java.io.FileWriter(rt.logFile, true)
    } catch (e: Exception) { null }

    private fun killTree(p: Process) {
        try {
            p.destroy()
            if (!ProcessCompat.waitFor(p, 3_000)) ProcessCompat.destroyForcibly(p)
            ProcessCompat.waitFor(p, 2_000)
        } catch (e: Exception) {
            try { ProcessCompat.destroyForcibly(p) } catch (e2: Exception) {}
        }
    }

    /** Set state and fire the callback OUTSIDE any lock. */
    private fun setState(s: State) { if (state != s) { state = s }; onState(s) }
    private fun log(s: String) { Log.i(TAG, s); onLogLine("[supervisor] $s") }

    companion object {
        private const val TAG = "zerodroid-gateway"
        private const val HEALTHY_MS = 20_000L
        private const val BACKOFF_BASE_MS = 1_000L
        private const val BACKOFF_CAP_MS = 60_000L
        private const val LOG_MAX_BYTES = 512L * 1024
    }
}

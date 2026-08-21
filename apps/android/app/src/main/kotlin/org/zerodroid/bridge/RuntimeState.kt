package org.zerodroid.bridge

import java.util.concurrent.CopyOnWriteArrayList

/**
 * In-process state bus between the foreground service (which owns the gateway) and the UI.
 * Survives Activity recreation; the service is the single writer.
 */
object RuntimeState {

    interface Listener {
        fun onGatewayState(state: GatewayProcess.State) {}
        fun onLog(line: String) {}
        fun onInfo() {}   // pairing code / urls / pid changed
    }

    private val listeners = CopyOnWriteArrayList<Listener>()
    private val logRing = ArrayDeque<String>()
    private const val LOG_CAP = 400

    @Volatile var gatewayState: GatewayProcess.State = GatewayProcess.State.STOPPED; private set
    @Volatile var pairCode: String? = null
    @Volatile var dashboardUrl: String? = null
    @Volatile var lanUrl: String? = null
    @Volatile var pid: Int? = null

    fun addListener(l: Listener) { listeners.addIfAbsent(l) }
    fun removeListener(l: Listener) { listeners.remove(l) }

    fun setGatewayState(s: GatewayProcess.State) {
        gatewayState = s
        listeners.forEach { it.onGatewayState(s) }
    }

    fun pushLog(line: String) {
        synchronized(logRing) {
            logRing.addLast(line)
            while (logRing.size > LOG_CAP) logRing.removeFirst()
        }
        listeners.forEach { it.onLog(line) }
    }

    fun logSnapshot(): List<String> = synchronized(logRing) { logRing.toList() }

    fun setInfo(pair: String? = pairCode, dash: String? = dashboardUrl, lan: String? = lanUrl, p: Int? = pid) {
        pairCode = pair; dashboardUrl = dash; lanUrl = lan; pid = p
        listeners.forEach { it.onInfo() }
    }
}

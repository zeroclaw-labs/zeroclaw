package org.zerodroid.bridge

import android.os.Build
import org.apache.sshd.common.config.keys.AuthorizedKeyEntry
import org.apache.sshd.common.config.keys.PublicKeyEntryResolver
import org.apache.sshd.server.Environment
import org.apache.sshd.server.ExitCallback
import org.apache.sshd.server.SshServer
import org.apache.sshd.server.channel.ChannelSession
import org.apache.sshd.server.command.Command
import org.apache.sshd.server.keyprovider.SimpleGeneratorHostKeyProvider
import org.apache.sshd.server.shell.ShellFactory
import java.io.File
import java.io.InputStream
import java.io.OutputStream
import java.security.PublicKey

/**
 * In-process SSH server (Apache MINA SSHD). Replaces the dropbear native binary: runs as the app
 * inside the foreground service, so there are NO setuid/setgid/seccomp problems (dropbear's
 * privilege-drop gets killed by Android seccomp on non-root). Pubkey-only; the shell is the
 * bundled busybox userspace with the agent's env. NIO2 transport requires API 26+.
 */
class SshdServer(private val rt: NativeRuntime, private val onLog: (String) -> Unit) {

    @Volatile private var sshd: SshServer? = null
    val running get() = sshd?.isOpen == true

    private val dbg = File(rt.home.parentFile, "ssh-debug.log")
    private fun d(s: String) {
        onLog("[ssh] $s")
        try { dbg.appendText("${System.currentTimeMillis()} $s\n") } catch (e: Exception) {}
    }

    fun start(port: Int, authorizedKeysText: String) {
        stop()
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) {
            d("SSH requires Android 8 / API 26 or newer — not started")
            return
        }
        // Accept keys from the UI field AND from ~/.ssh/authorized_keys on disk (so a key can be
        // dropped in by file too, not only the app UI).
        val fileText = try { rt.sshAuthorizedKeys.takeIf { it.exists() }?.readText() ?: "" } catch (e: Exception) { "" }
        val keys = parseAuthorizedKeys(authorizedKeysText + "\n" + fileText)
        d("parsed ${keys.size} key(s) (fileLen=${fileText.length} cfgLen=${authorizedKeysText.length})")
        if (keys.isEmpty()) { d("no valid authorized key — not started"); return }
        rt.sshDir.mkdirs()
        val s = SshServer.setUpDefaultServer()
        s.port = port
        s.host = "0.0.0.0"
        s.keyPairProvider = SimpleGeneratorHostKeyProvider(File(rt.sshDir, "mina_host_key.ser").toPath())
        s.publickeyAuthenticator = org.apache.sshd.server.auth.pubkey.PublickeyAuthenticator { user, offered, _ ->
            // Compare key MATERIAL (KeyUtils handles differing PublicKey impls/providers; plain
            // equals() fails across the eddsa provider vs MINA's wire-parsed key).
            val ok = keys.any { org.apache.sshd.common.config.keys.KeyUtils.compareKeys(it, offered) }
            android.util.Log.i("zerodroid-ssh", "auth user=$user offered=${offered.algorithm}/${offered.javaClass.simpleName} vs ${keys.size} keys -> $ok")
            ok
        }
        s.shellFactory = ShellFactory { _ -> ShellCommand(rt) }
        s.addSessionListener(object : org.apache.sshd.common.session.SessionListener {
            override fun sessionException(session: org.apache.sshd.common.session.Session, t: Throwable) {
                d("session EXC: ${t.javaClass.simpleName}: ${t.message}")
            }
            override fun sessionClosed(session: org.apache.sshd.common.session.Session) { d("session closed") }
        })
        s.start()
        sshd = s
        d("MINA listening on 0.0.0.0:$port")
    }

    fun stop() {
        try { sshd?.stop(true) } catch (e: Exception) {}
        sshd = null
    }

    private fun parseAuthorizedKeys(text: String): List<PublicKey> = text.lineSequence().mapNotNull { line ->
        var t = line.trim()
        if (t.isEmpty() || t.startsWith("#")) return@mapNotNull null
        // On-screen keyboards auto-capitalize the first letter ("Ssh-ed25519"); key types are
        // lowercase, so normalize the leading algorithm token.
        val sp = t.indexOf(' ')
        if (sp > 0) t = t.substring(0, sp).lowercase() + t.substring(sp)
        try {
            AuthorizedKeyEntry.parseAuthorizedKeyEntry(t)
                .resolvePublicKey(null, java.util.Collections.emptyMap(), PublicKeyEntryResolver.IGNORING)
        } catch (e: Exception) { onLog("[ssh] bad authorized key: ${e.message}"); null }
    }.toList()

    /** A shell channel backed by the bundled busybox userspace, with the agent's env (PATH/HOME/…). */
    private class ShellCommand(private val rt: NativeRuntime) : Command {
        private var input: InputStream? = null
        private var output: OutputStream? = null
        private var error: OutputStream? = null
        private var exit: ExitCallback? = null
        private var proc: Process? = null

        override fun setInputStream(i: InputStream) { input = i }
        override fun setOutputStream(o: OutputStream) { output = o }
        override fun setErrorStream(e: OutputStream) { error = e }
        override fun setExitCallback(c: ExitCallback) { exit = c }

        override fun start(channel: ChannelSession, env: Environment) {
            val sh = File(rt.binDir, "sh").takeIf { it.exists() }?.absolutePath ?: "/system/bin/sh"
            val pb = ProcessBuilder(sh, "-i").directory(rt.home).redirectErrorStream(false)
            rt.applyEnv(pb)
            val p = pb.start(); proc = p
            pump(input, p.outputStream, closeDst = true)
            pump(p.inputStream, output, closeDst = false)
            pump(p.errorStream, error, closeDst = false)
            Thread({
                val code = try { p.waitFor() } catch (e: Exception) { 1 }
                try { output?.flush(); error?.flush() } catch (e: Exception) {}
                exit?.onExit(code)
            }, "ssh-wait").apply { isDaemon = true }.start()
        }

        override fun destroy(channel: ChannelSession) {
            try { proc?.let { ProcessCompat.destroyForcibly(it) } } catch (e: Exception) {}
        }

        private fun pump(src: InputStream?, dst: OutputStream?, closeDst: Boolean) {
            if (src == null || dst == null) return
            Thread({
                try {
                    val buf = ByteArray(8192); var n: Int
                    while (src.read(buf).also { n = it } >= 0) { dst.write(buf, 0, n); dst.flush() }
                } catch (e: Exception) {} finally { if (closeDst) try { dst.close() } catch (e: Exception) {} }
            }, "ssh-pump").apply { isDaemon = true }.start()
        }
    }
}

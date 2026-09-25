package org.zerodroid.bridge

import android.content.Context
import android.util.Log
import java.io.File

/**
 * Owns the on-device layout for the bundled rust agent.
 *
 * W^X: Android 10+ refuses to exec() anything in the app's writable data dir. The only
 * APK-shipped files that stay executable are the native libraries, extracted by the package
 * manager into [Context.getApplicationInfo].nativeLibraryDir. So the zeroclaw gateway binary and
 * busybox ship as jniLibs `lib*.so` and are exec'd directly from there — never copied into filesDir.
 *
 * Everything writable (config, agent workspace, web dashboard, busybox applet symlinks) lives under
 * filesDir, which is app-private and not world-readable.
 */
class NativeRuntime(private val ctx: Context, variant: String = "") {

    private val nativeDir = File(ctx.applicationInfo.nativeLibraryDir)

    /** The rust agent/gateway binary, exec-able from the native lib dir. */
    val zeroclawBin = File(nativeDir, "libzeroclaw.so")
    /** Optional static BusyBox (extra userspace for the agent's shell tool). */
    val busyboxBin = File(nativeDir, "libbusybox.so")

    private val homeName = if (variant.isBlank()) "zeroclaw-home" else "zeroclaw-home-$variant"
    /** HOME for the gateway process — agent workspace + memory live under here. Variant-isolated so
     *  e.g. the provider self-test never collides with the live gateway's config dir. */
    val home = File(ctx.filesDir, homeName)
    /** zeroclaw config dir (passed via --config-dir). */
    val configDir = File(home, ".zeroclaw")
    val configFile = File(configDir, "config.toml")
    /** busybox applet symlinks; first on the agent shell PATH. SHARED across variants (read-only
     *  symlinks into the native lib dir). */
    val binDir = File(ctx.filesDir, "bin")
    /** Web dashboard static assets (gateway.web_dist_dir), extracted from APK assets if present. */
    val webDist = File(ctx.filesDir, "web-dist")
    /** Gateway stdout/stderr log file (also surfaced in the UI). */
    val logFile = File(ctx.filesDir, "gateway.log")

    val gatewayPresent get() = zeroclawBin.exists()
    // ---- SSH layout (used by the in-process Apache MINA server) ----
    val sshDir = File(home, ".ssh")
    val sshAuthorizedKeys = File(sshDir, "authorized_keys")

    /** Argv for the supervised gateway process. */
    fun gatewayArgv() = listOf(
        zeroclawBin.absolutePath, "gateway", "start", "--config-dir", configDir.absolutePath, "--log-level", "info")

    /** Write the user's public key as authorized_keys (0600). */
    fun writeAuthorizedKeys(pubkey: String) {
        sshDir.mkdirs()
        try { android.system.Os.chmod(sshDir.absolutePath, 448) } catch (e: Exception) {}
        sshAuthorizedKeys.writeText(if (pubkey.endsWith("\n")) pubkey else pubkey + "\n")
        try { android.system.Os.chmod(sshAuthorizedKeys.absolutePath, 384 /* 0600 */) } catch (e: Exception) {}
    }

    /** The app's own uid name. Prefer optional BusyBox, otherwise use Android's system `id`. */
    fun uidName(): String {
        val idBinary = File(binDir, "id").takeIf { it.exists() } ?: File("/system/bin/id")
        return execWithTimeout(listOf(idBinary.absolutePath, "-un"), 4_000) { applyEnv(it) }
            ?.trim()?.lineSequence()?.firstOrNull()?.takeIf { it.isNotBlank() } ?: "shell"
    }

    /** Idempotent first-run setup. Safe to call on every service start. */
    fun prepare(agentAlias: String = "phone") {
        configDir.mkdirs()
        // zeroclaw does not auto-create the per-agent workspace; the shell tool's spawn cwd is it,
        // so a missing dir is an ENOENT before any command runs. Create it up front.
        File(configDir, "agents/$agentAlias/workspace").mkdirs()
        binDir.mkdirs()
        File(home, "tmp").mkdirs()
        setupBusyboxApplets()
        extractWebDist()
        extractSkills()
    }

    /**
     * Drop the bundled skills (assets/skills/<name>/SKILL.md) into the agent's skills dir so the
     * agent knows its Android powers out of the box — fire Intents, read sensors/SMS, call the
     * bridge. zeroclaw discovers skills under <config-dir>/data/skills/<name>/. Idempotent: only
     * writes a skill whose SKILL.md is missing (never clobbers a user edit).
     */
    private fun extractSkills() {
        val names = try { ctx.assets.list("skills")?.toList() ?: emptyList() } catch (e: Exception) { emptyList() }
        if (names.isEmpty()) return
        val skillsRoot = File(configDir, "data/skills")
        for (name in names) {
            val dest = File(skillsRoot, "$name/SKILL.md")
            if (dest.exists()) continue
            dest.parentFile?.mkdirs()
            try {
                ctx.assets.open("skills/$name/SKILL.md").use { input ->
                    dest.outputStream().use { input.copyTo(it) }
                }
                Log.i(TAG, "bundled skill installed: $name")
            } catch (e: Exception) { Log.w(TAG, "skill $name: ${e.message}") }
        }
    }

    /**
     * busybox is one binary that dispatches on argv[0]. Give the agent's shell real applet names by
     * symlinking each applet in [binDir] back to the executable inode in the native lib dir. The
     * symlink itself lives in writable filesDir but resolves to an executable target, sidestepping
     * W^X. If the filesystem refuses symlinks we fall back to PATH-only (agent uses `busybox <app>`).
     */
    private fun setupBusyboxApplets() {
        if (!busyboxBin.exists()) return
        val marker = File(binDir, ".applets-linked-${busyboxBin.lastModified()}")
        // Self-healing: only skip if the marker AND a required applet symlink both exist.
        if (marker.exists() && File(binDir, "sh").exists()) return
        binDir.listFiles()?.forEach { it.delete() }
        // busybox dispatches on argv[0]: `busybox --list` only works when invoked AS "busybox".
        // Create that symlink first, then enumerate applets through it.
        val busyboxLink = File(binDir, "busybox")
        try { android.system.Os.symlink(busyboxBin.absolutePath, busyboxLink.absolutePath) } catch (e: Exception) {}
        val applets = listBusyboxApplets(busyboxLink).ifEmpty { DEFAULT_APPLETS }
        var linked = 0
        for (app in applets) {
            val a = app.trim()
            if (a.isEmpty()) continue
            val link = File(binDir, a)
            try {
                if (link.exists()) link.delete()
                android.system.Os.symlink(busyboxBin.absolutePath, link.absolutePath)
                linked++
            } catch (e: Exception) { /* fs without symlink support — PATH fallback covers it */ }
        }
        if (linked > 0 && File(binDir, "sh").exists()) marker.createNewFile()
        Log.i(TAG, "busybox applets linked: $linked/${applets.size}")
    }

    /** `busybox --list` (invoked via the busybox-named symlink) with a HARD timeout. Only valid
     *  applet names are kept; any error text is filtered out. */
    private fun listBusyboxApplets(busyboxLink: File): List<String> {
        val exe = if (busyboxLink.exists()) busyboxLink.absolutePath else busyboxBin.absolutePath
        val out = execWithTimeout(listOf(exe, "--list"), 5_000) ?: return emptyList()
        // applet names are short, no spaces/slashes — drop any error lines.
        return out.lineSequence().map { it.trim() }.filter {
            it.isNotEmpty() && it.length <= 16 &&
                it.all { c -> c.isLetterOrDigit() || c == '_' || c == '.' || c == '[' || c == ']' }
        }.toList()
    }

    /**
     * Run a short-lived child and return merged stdout/stderr, or null on timeout/error. The read
     * runs on its own thread so a child that never closes stdout still hits the wall-clock timeout
     * (a plain readText() before waitFor() would block forever — the bug this avoids).
     */
    fun execWithTimeout(cmd: List<String>, timeoutMs: Long, env: ((ProcessBuilder) -> Unit)? = null): String? =
        execStatus(cmd, timeoutMs, env).second

    /** Like [execWithTimeout] but returns (exitCode, mergedOutput); exitCode is null on timeout/error
     *  (so callers can branch on success rather than guessing from output text). */
    fun execStatus(cmd: List<String>, timeoutMs: Long, env: ((ProcessBuilder) -> Unit)? = null): Pair<Int?, String?> {
        var p: Process? = null
        return try {
            val pb = ProcessBuilder(cmd).directory(home).redirectErrorStream(true)
            env?.invoke(pb)
            p = pb.start()
            val sb = StringBuilder()
            val reader = Thread {
                try { p!!.inputStream.bufferedReader().forEachLine { sb.appendLine(it) } } catch (e: Exception) {}
            }.apply { isDaemon = true; start() }
            val exited = ProcessCompat.waitFor(p, timeoutMs)
            if (!exited) {
                ProcessCompat.destroyForcibly(p)
                reader.join(500)
                return null to sb.toString()
            }
            reader.join(500)
            p.exitValue() to sb.toString()
        } catch (e: Exception) {
            Log.w(TAG, "exec ${cmd.firstOrNull()} failed: ${e.message}")
            p?.let { ProcessCompat.destroyForcibly(it) }
            null to null
        }
    }

    /** Copy the bundled web dashboard (the assets/web-dist tree) into filesDir. No-op if absent. */
    private fun extractWebDist() {
        val am = ctx.assets
        val top = try { am.list("web-dist")?.toList() ?: emptyList() } catch (e: Exception) { emptyList() }
        if (top.isEmpty()) return
        val verAsset = "web-dist/.version"
        val bundledVer = try { am.open(verAsset).bufferedReader().readText().trim() } catch (e: Exception) { "" }
        val localVerFile = File(webDist, ".version")
        if (webDist.exists() && bundledVer.isNotEmpty() && localVerFile.exists() &&
            localVerFile.readText().trim() == bundledVer) return
        webDist.deleteRecursively()
        webDist.mkdirs()
        copyAsset("web-dist")
        Log.i(TAG, "web-dist extracted (ver=$bundledVer)")
    }

    private fun copyAsset(path: String) {
        val am = ctx.assets
        val children = am.list(path) ?: return
        if (children.isEmpty()) {
            // leaf file (or an empty asset dir — guard the open so it can't crash startup)
            try {
                val out = File(ctx.filesDir, path)
                out.parentFile?.mkdirs()
                am.open(path).use { input -> out.outputStream().use { input.copyTo(it) } }
            } catch (e: Exception) { Log.w(TAG, "skip asset $path: ${e.message}") }
        } else {
            File(ctx.filesDir, path).mkdirs()
            for (c in children) copyAsset("$path/$c")
        }
    }

    /** Enumerate the bundled binary's supported model providers (`zeroclaw providers`, 61+).
     *  Falls back to a curated short list if the binary can't be queried. */
    fun listProviders(): List<ProviderInfo> {
        if (!gatewayPresent) return ProviderCatalog.FALLBACK
        val out = execWithTimeout(listOf(zeroclawBin.absolutePath, "providers"), 8_000) ?: return ProviderCatalog.FALLBACK
        val rows = ArrayList<ProviderInfo>()
        for (raw in out.lineSequence()) {
            val line = raw.trimEnd()
            // provider rows are "  <id>   <description>"; the id is a lowercase token, no spaces.
            val m = Regex("^\\s{2,}([a-z0-9_]+)\\s{2,}(\\S.*)$").find(line) ?: continue
            val id = m.groupValues[1]
            if (id == "id" || id == "use") continue          // header
            var desc = m.groupValues[2].trim()
            val local = desc.contains("[local]")
            desc = desc.replace("[local]", "").replace("(configured)", "").trim()
            rows += ProviderInfo(id, desc.ifEmpty { id }, local || ProviderCatalog.isLocal(id))
        }
        return ProviderCatalog.orderForPicker(rows.ifEmpty { ProviderCatalog.FALLBACK })
    }

    /** Mirror the bridge token into the agent's HOME so its shell tool can authenticate to 8470. */
    fun publishBridgeToken(token: String) {
        try {
            val file = File(configDir, "bridge-token")
            file.writeText(token)
            android.system.Os.chmod(file.absolutePath, 384 /* 0600 */)
        } catch (e: Exception) {}
    }

    /**
     * Configure the child's environment by INHERITING the Android baseline (ANDROID_ROOT,
     * ANDROID_DATA, EXTERNAL_STORAGE, etc. — bionic/tools rely on these) and overriding only what
     * we need. Clearing the env entirely would strip those and break some tools.
     */
    fun applyEnv(pb: ProcessBuilder) {
        val env = pb.environment()   // starts from the inherited process env
        env["HOME"] = home.absolutePath
        // Deliberate order: busybox applets, then the native dir (raw binaries), then system.
        env["PATH"] = "${binDir.absolutePath}:${nativeDir.absolutePath}:/system/bin:/system/xbin"
        // The bionic build links libc dynamically; a separately exec'd process needs this.
        env["LD_LIBRARY_PATH"] = nativeDir.absolutePath
        env["TMPDIR"] = File(home, "tmp").absolutePath
        env["ZEROCLAW_BUSYBOX"] = busyboxBin.absolutePath
        env["LANG"] = "C.UTF-8"
    }

    companion object {
        private const val TAG = "zerodroid-runtime"
        private val DEFAULT_APPLETS = listOf(
            "sh", "ash", "ls", "cat", "cp", "mv", "rm", "mkdir", "chmod", "ln", "echo", "pwd",
            "grep", "sed", "awk", "head", "tail", "wc", "find", "sort", "uniq", "cut", "tr",
            "curl", "wget", "nc", "telnet", "ping", "ps", "kill", "df", "du", "date", "env",
            "base64", "md5sum", "sha256sum", "tar", "gzip", "gunzip", "unzip", "xxd", "which"
        )
    }
}

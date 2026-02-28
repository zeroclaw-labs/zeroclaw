package ai.zeroclaw.android.data

import android.content.Context
import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.*
import androidx.datastore.preferences.preferencesDataStore
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.flow.map
import java.io.IOException

// Extension for DataStore
private val Context.dataStore: DataStore<Preferences> by preferencesDataStore(name = "zeroclaw_settings")

/**
 * Repository for persisting ZeroClaw settings.
 *
 * Uses DataStore for general settings and EncryptedSharedPreferences
 * for sensitive data like API keys.
 */
class SettingsRepository(private val context: Context) {

    // DataStore keys
    private object Keys {
        val PROVIDER = stringPreferencesKey("provider")
        val MODEL = stringPreferencesKey("model")
        val AUTO_START = booleanPreferencesKey("auto_start")
        val NOTIFICATIONS_ENABLED = booleanPreferencesKey("notifications_enabled")
        val SYSTEM_PROMPT = stringPreferencesKey("system_prompt")
        val HEARTBEAT_INTERVAL = intPreferencesKey("heartbeat_interval")
        val FIRST_RUN = booleanPreferencesKey("first_run")
    }

    // Encrypted storage for API key
    private val encryptedPrefs by lazy {
        val masterKey = MasterKey.Builder(context)
            .setKeyScheme(MasterKey.KeyScheme.AES256_GCM)
            .build()

        EncryptedSharedPreferences.create(
            context,
            "zeroclaw_secure",
            masterKey,
            EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
            EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM
        )
    }

    // Flow of settings with IOException handling for DataStore corruption
    val settings: Flow<ZeroClawSettings> = context.dataStore.data
        .catch { exception ->
            if (exception is IOException) {
                android.util.Log.e("SettingsRepository", "Error reading DataStore", exception)
                emit(emptyPreferences())
            } else {
                throw exception
            }
        }
        .map { prefs ->
        ZeroClawSettings(
            provider = prefs[Keys.PROVIDER] ?: "anthropic",
            model = prefs[Keys.MODEL] ?: "claude-sonnet-4-5",
            apiKey = getApiKey(),
            autoStart = prefs[Keys.AUTO_START] ?: false,
            notificationsEnabled = prefs[Keys.NOTIFICATIONS_ENABLED] ?: true,
            systemPrompt = prefs[Keys.SYSTEM_PROMPT] ?: "",
            heartbeatIntervalMinutes = prefs[Keys.HEARTBEAT_INTERVAL] ?: 15
        )
    }

    val isFirstRun: Flow<Boolean> = context.dataStore.data
        .catch { exception ->
            if (exception is IOException) {
                android.util.Log.e("SettingsRepository", "Error reading DataStore", exception)
                emit(emptyPreferences())
            } else {
                throw exception
            }
        }
        .map { prefs ->
            prefs[Keys.FIRST_RUN] ?: true
        }

    suspend fun updateSettings(settings: ZeroClawSettings) {
        // Save API key to encrypted storage
        saveApiKey(settings.apiKey)

        // Save other settings to DataStore
        context.dataStore.edit { prefs ->
            prefs[Keys.PROVIDER] = settings.provider
            prefs[Keys.MODEL] = settings.model
            prefs[Keys.AUTO_START] = settings.autoStart
            prefs[Keys.NOTIFICATIONS_ENABLED] = settings.notificationsEnabled
            prefs[Keys.SYSTEM_PROMPT] = settings.systemPrompt
            prefs[Keys.HEARTBEAT_INTERVAL] = settings.heartbeatIntervalMinutes
        }
    }

    suspend fun setFirstRunComplete() {
        context.dataStore.edit { prefs ->
            prefs[Keys.FIRST_RUN] = false
        }
    }

    suspend fun updateProvider(provider: String) {
        context.dataStore.edit { prefs ->
            prefs[Keys.PROVIDER] = provider
        }
    }

    suspend fun updateModel(model: String) {
        context.dataStore.edit { prefs ->
            prefs[Keys.MODEL] = model
        }
    }

    suspend fun updateAutoStart(enabled: Boolean) {
        context.dataStore.edit { prefs ->
            prefs[Keys.AUTO_START] = enabled
        }
    }

    // Encrypted API key storage
    private fun saveApiKey(apiKey: String) {
        encryptedPrefs.edit().putString("api_key", apiKey).apply()
    }

    private fun getApiKey(): String {
        return encryptedPrefs.getString("api_key", "") ?: ""
    }

    fun hasApiKey(): Boolean {
        return getApiKey().isNotBlank()
    }

    fun clearApiKey() {
        encryptedPrefs.edit().remove("api_key").apply()
    }
}

/**
 * Settings data class with all configurable options
 */
data class ZeroClawSettings(
    val provider: String = "anthropic",
    val model: String = "claude-sonnet-4-5",
    val apiKey: String = "",
    val autoStart: Boolean = false,
    val notificationsEnabled: Boolean = true,
    val systemPrompt: String = "",
    val heartbeatIntervalMinutes: Int = 15
) {
    fun isConfigured(): Boolean = apiKey.isNotBlank()
}

package dev.buildmesh.remote

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.AtomicFile
import java.security.KeyStore
import java.util.Base64
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

data class DeviceSession(val origin: String, val root: ByteArray?, val cookie: String)

/** Device credentials stay in app-private, backup-excluded storage encrypted by Android Keystore. */
class SessionStore(context: Context) {
    private val file = AtomicFile(context.filesDir.resolve("device-session"))
    private val alias = "buildmesh.device-session.v1"

    @Synchronized
    private fun key(): SecretKey {
        val keys = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        (keys.getKey(alias, null) as? SecretKey)?.let { return it }
        return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore").apply {
            init(KeyGenParameterSpec.Builder(alias, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM).setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE).build())
        }.generateKey()
    }

    @Synchronized
    fun read(): DeviceSession? {
        if (!file.baseFile.exists()) return null
        return try {
            val bytes = file.readFully()
            val cipher = Cipher.getInstance("AES/GCM/NoPadding").apply {
                init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(128, bytes.copyOfRange(0, 12)))
            }
            val json = Json.parseToJsonElement(String(cipher.doFinal(bytes.copyOfRange(12, bytes.size)), Charsets.UTF_8)).obj()
            DeviceSession(json.text("origin"), json.text("root").takeIf { it.isNotEmpty() }?.let { Base64.getDecoder().decode(it) }, json.text("cookie"))
        } catch (_: Exception) {
            clear()
            null
        }
    }

    @Synchronized
    fun save(session: DeviceSession) {
        val json = buildJsonObject {
            put("origin", session.origin)
            put("root", session.root?.let { Base64.getEncoder().encodeToString(it) }.orEmpty())
            put("cookie", session.cookie)
        }.toString().toByteArray(Charsets.UTF_8)
        val cipher = Cipher.getInstance("AES/GCM/NoPadding").apply { init(Cipher.ENCRYPT_MODE, key()) }
        val stream = file.startWrite()
        try {
            stream.write(cipher.iv + cipher.doFinal(json))
            file.finishWrite(stream)
        } catch (e: Exception) {
            file.failWrite(stream)
            throw e
        }
    }

    @Synchronized
    fun clear() { file.delete() }
}

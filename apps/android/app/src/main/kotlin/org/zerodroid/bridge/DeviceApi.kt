package org.zerodroid.bridge

import android.annotation.SuppressLint
import android.content.Context
import android.content.pm.PackageManager
import android.hardware.Sensor
import android.hardware.SensorManager
import android.telephony.TelephonyManager
import androidx.core.content.ContextCompat
import com.google.android.gms.location.LocationServices
import org.json.JSONArray
import org.json.JSONObject
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

/**
 * Read-only device facts, shared by the two front doors.
 *
 * These used to be private methods on [ApiServer], reachable only over the loopback HTTP surface.
 * The UI tools reach the phone over a UID-isolated Unix socket instead, because a loopback TCP
 * port is reachable by every app on the device and a bearer token is then the only thing guarding
 * it. Device facts are at least as sensitive as screen control — location and telephony identify
 * the owner — so the same reasoning applies, and [UiSocketServer] needs to answer them without
 * going back out through a port.
 *
 * Pure code motion: [ApiServer] still serves the same JSON on the same paths, and now delegates
 * here rather than owning the implementations.
 */
object DeviceApi {

    fun granted(ctx: Context, permission: String) =
        ContextCompat.checkSelfPermission(ctx, permission) == PackageManager.PERMISSION_GRANTED

    fun sensors(ctx: Context): JSONObject {
        val sm = ctx.getSystemService(Context.SENSOR_SERVICE) as SensorManager
        val arr = JSONArray()
        for (s in sm.getSensorList(Sensor.TYPE_ALL))
            arr.put(
                JSONObject().put("name", s.name).put("type", s.stringType)
                    .put("vendor", s.vendor).put("power_mA", s.power.toDouble())
            )
        return JSONObject().put("count", arr.length()).put("sensors", arr)
    }

    /**
     * Last known fused location. Blocks briefly on a callback, so never call it from the main
     * thread; the socket server already answers on a worker.
     */
    @SuppressLint("MissingPermission") // ACCESS_FINE_LOCATION is checked before the GMS call.
    fun location(ctx: Context): JSONObject {
        if (!granted(ctx, "android.permission.ACCESS_FINE_LOCATION"))
            return JSONObject().put("error", "ACCESS_FINE_LOCATION not granted")
        val out = JSONObject()
        val latch = CountDownLatch(1)
        LocationServices.getFusedLocationProviderClient(ctx).lastLocation
            .addOnSuccessListener { loc ->
                if (loc != null) out.put("lat", loc.latitude).put("lon", loc.longitude)
                    .put("accuracy_m", loc.accuracy.toDouble()).put("provider", loc.provider)
                else out.put("note", "no last location yet")
                latch.countDown()
            }.addOnFailureListener { e -> out.put("error", e.message); latch.countDown() }
        latch.await(6, TimeUnit.SECONDS)
        return out
    }

    fun telephony(ctx: Context): JSONObject {
        val tm = ctx.getSystemService(Context.TELEPHONY_SERVICE) as TelephonyManager
        return JSONObject().put("network_operator", tm.networkOperatorName)
            .put("sim_operator", tm.simOperatorName).put("country", tm.networkCountryIso)
            .put("phone_type", tm.phoneType).put("data_state", tm.dataState)
    }
}

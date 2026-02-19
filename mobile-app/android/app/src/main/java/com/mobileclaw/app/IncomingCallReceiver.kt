package com.mobileclaw.app

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.telephony.TelephonyManager

class IncomingCallReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != TelephonyManager.ACTION_PHONE_STATE_CHANGED) return

        val state = intent.getStringExtra(TelephonyManager.EXTRA_STATE).orEmpty()
        if (state != TelephonyManager.EXTRA_STATE_RINGING) return

        val number = intent.getStringExtra(TelephonyManager.EXTRA_INCOMING_NUMBER)
        AndroidAgentToolsModule.emitIncomingCall(context, "ringing", number)
    }
}

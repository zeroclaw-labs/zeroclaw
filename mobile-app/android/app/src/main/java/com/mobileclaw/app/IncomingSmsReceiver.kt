package com.mobileclaw.app

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.telephony.SmsMessage

class IncomingSmsReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != "android.provider.Telephony.SMS_RECEIVED") return
        val extras = intent.extras ?: return
        val pdus = extras.get("pdus") as? Array<*> ?: return
        val format = extras.getString("format")

        for (pdu in pdus) {
            val bytes = pdu as? ByteArray ?: continue
            val sms = SmsMessage.createFromPdu(bytes, format)
            AndroidAgentToolsModule.emitIncomingSms(context, sms.originatingAddress, sms.messageBody)
        }
    }
}

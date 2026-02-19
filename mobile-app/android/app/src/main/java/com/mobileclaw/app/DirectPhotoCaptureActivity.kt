package com.mobileclaw.app

import android.os.Bundle
import androidx.appcompat.app.AppCompatActivity
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageCapture
import androidx.camera.core.ImageCaptureException
import androidx.camera.core.Preview
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.core.content.ContextCompat
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import java.io.File

class DirectPhotoCaptureActivity : AppCompatActivity() {
    private lateinit var previewView: PreviewView
    private var imageCapture: ImageCapture? = null
    private var done = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        previewView = PreviewView(this)
        setContentView(previewView)

        startCameraAndCapture()
    }

    private fun startCameraAndCapture() {
        val providerFuture = ProcessCameraProvider.getInstance(this)
        providerFuture.addListener(
            {
                val cameraProvider = providerFuture.get()
                val lens = intent.getStringExtra("lens")?.trim()?.lowercase(Locale.US) ?: "rear"
                val selector = if (lens == "front") {
                    CameraSelector.DEFAULT_FRONT_CAMERA
                } else {
                    CameraSelector.DEFAULT_BACK_CAMERA
                }

                val preview = Preview.Builder().build().also { it.setSurfaceProvider(previewView.getSurfaceProvider()) }
                imageCapture = ImageCapture.Builder()
                    .setCaptureMode(ImageCapture.CAPTURE_MODE_MINIMIZE_LATENCY)
                    .build()

                cameraProvider.unbindAll()
                cameraProvider.bindToLifecycle(this, selector, preview, imageCapture)

                previewView.postDelayed({ takePhoto() }, 450)
            },
            ContextCompat.getMainExecutor(this),
        )
    }

    private fun takePhoto() {
        val capture = imageCapture ?: run {
            finishSafe()
            return
        }

        val timestamp = SimpleDateFormat("yyyyMMdd_HHmmss", Locale.US).format(Date())
        val mediaDir = File(filesDir, "runtime/media")
        mediaDir.mkdirs()
        val outputFile = File(mediaDir, "mobileclaw_${timestamp}.jpg")

        val output = ImageCapture.OutputFileOptions
            .Builder(outputFile)
            .build()

        capture.takePicture(
            output,
            ContextCompat.getMainExecutor(this),
            object : ImageCapture.OnImageSavedCallback {
                override fun onImageSaved(outputFileResults: ImageCapture.OutputFileResults) {
                    val localPath = outputFile.absolutePath
                    RuntimeBridge.recordPhotoCaptured(this@DirectPhotoCaptureActivity, localPath)
                    AndroidAgentToolsModule.emitPhotoCaptured(localPath)
                    finishSafe()
                }

                override fun onError(exception: ImageCaptureException) {
                    finishSafe()
                }
            },
        )
    }

    private fun finishSafe() {
        if (done) return
        done = true
        finish()
    }
}

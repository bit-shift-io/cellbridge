package dev.cellbridge

import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import java.net.HttpURLConnection
import java.net.URL

class AndroidBridgeNetworks(private val cm: ConnectivityManager) : BridgeNetworks {
    private val binder = NetworkBinder(cm)
    override fun start(changed: () -> Unit) {
        binder.events = object : NetworkEvents {
            override fun onCellularChanged(up: Boolean) = changed()
            override fun onWifiChanged(up: Boolean) = changed()
        }
        binder.start()
    }
    override fun current(): NetworkSelection? {
        val cellular = binder.cellularNetwork() ?: return null
        val wifi = binder.wifiNetwork() ?: return null
        return NetworkSelection(cellular, wifi)
    }
    override fun bindCellular(network: Network): Boolean {
        val caps = cm.getNetworkCapabilities(network) ?: return false
        if (!caps.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR) ||
            !caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET) || caps.hasTransport(NetworkCapabilities.TRANSPORT_VPN)) return false
        return binder.bindProcess(network) && cm.boundNetworkForProcess == network
    }
    override fun wifiSocket(network: Network): Int {
        check(cm.getNetworkCapabilities(network)?.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) == true)
        return binder.bindWifiSocket(network)
    }
    override fun unbind() { cm.bindProcessToNetwork(null) }
    override fun stop() { binder.stop(); binder.events = null }
    override fun cellularControl(network: Network): String {
        check(cm.getNetworkCapabilities(network)?.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR) == true)
        val connection = network.openConnection(URL("https://api4.ipify.org")) as HttpURLConnection
        try {
            connection.connectTimeout = 5000; connection.readTimeout = 5000
            return connection.inputStream.use { input ->
                val output = java.io.ByteArrayOutputStream()
                val buffer = ByteArray(128)
                while (output.size() < 1024) {
                    val n = input.read(buffer, 0, minOf(buffer.size, 1024 - output.size()))
                    if (n < 0) break
                    output.write(buffer, 0, n)
                }
                output.toString("UTF-8").trim()
            }
        } finally { connection.disconnect() }
    }
}

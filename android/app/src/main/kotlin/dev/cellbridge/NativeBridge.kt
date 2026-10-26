package dev.cellbridge

interface BridgeFacade {
    /** Ownership of fd transfers to Rust on entry, including failure. */
    fun start(configJson: String, fd: Int)
    fun stop()
    fun snapshotJson(): String
}

class NativeBridge : BridgeFacade {
    init { System.loadLibrary("cellbridge") }
    private external fun nativeStart(config: String, fd: Int)
    private external fun nativeStop()
    private external fun nativeSnapshot(): String
    private external fun nativeTestConnection(config: String, fd: Int)
    private external fun nativeRouterSetup(request: String, fd: Int)
    private external fun nativeRouterSetupSnapshot(): String
    private external fun nativeCancelRouterSetup()
    fun routerSetup(request: String, fd: Int) {
        try { nativeRouterSetup(request, fd) }
        catch (error: UnsatisfiedLinkError) {
            android.os.ParcelFileDescriptor.adoptFd(fd).close()
            throw error
        }
    }
    fun routerSetupSnapshot(): String = nativeRouterSetupSnapshot()
    fun cancelRouterSetup() = nativeCancelRouterSetup()
    fun testConnection(configJson: String, fd: Int) {
        try { nativeTestConnection(configJson, fd) }
        catch (error: UnsatisfiedLinkError) {
            android.os.ParcelFileDescriptor.adoptFd(fd).close()
            throw error
        }
    }
    override fun start(configJson: String, fd: Int) {
        try { nativeStart(configJson, fd) }
        catch (error: UnsatisfiedLinkError) {
            // JNI entry was never reached, so Rust never acquired ownership.
            android.os.ParcelFileDescriptor.adoptFd(fd).close()
            throw error
        }
    }
    override fun stop() = nativeStop()
    override fun snapshotJson() = nativeSnapshot()
}

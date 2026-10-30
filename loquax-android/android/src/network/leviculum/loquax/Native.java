package network.leviculum.loquax;

/**
 * The Rust library, libloquax_android.so (the crate in this directory).
 * Every method here is implemented in loquax-android/src/lib.rs; the bytes
 * are msgpack in the schema of src/schema.rs, which {@link Core} reads.
 */
final class Native {
    static {
        System.loadLibrary("loquax_android");
    }

    private Native() {}

    /** The loquax version and this device's home node client state. */
    static native String describe(String home);

    /** Start the session; blocks while a first start joins. */
    static native byte[] start(byte[] request);

    /** One thing the user did. */
    static native byte[] command(byte[] request);

    /** What a screen shows. */
    static native byte[] query(byte[] request);

    /** The next event, or null when none came within waitMs. */
    static native byte[] nextEvent(int waitMs);

    /**
     * One frame of the map: its description's length (four bytes, big
     * endian), the description, then its RGBA pixels. Not on the UI thread.
     */
    static native byte[] renderMap(byte[] request);

    /** Stop the session. */
    static native void stop();
}

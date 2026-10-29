package network.leviculum.loquax;

/**
 * The Rust library, libloquax_android.so (the crate in this directory).
 * Every method here is implemented in loquax-android/src/lib.rs.
 */
final class Native {
    static {
        System.loadLibrary("loquax_android");
    }

    private Native() {}

    /** The loquax version and this device's home node client state. */
    static native String describe(String home);
}

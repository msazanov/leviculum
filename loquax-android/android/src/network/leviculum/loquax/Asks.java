package network.leviculum.loquax;

import android.Manifest;
import android.app.Activity;
import android.app.AlertDialog;
import android.content.DialogInterface;
import android.content.Intent;
import android.content.SharedPreferences;
import android.content.pm.PackageManager;
import android.net.Uri;
import android.os.Build;
import android.os.PowerManager;
import android.provider.Settings;

/**
 * What the app asks the user once, on the first conversation list after a
 * join, and never again whatever the answer: to show notifications
 * (Android 13 and later ask at run time), and to leave the app out of the
 * battery optimisation, so the doze does not cut the link to the home node
 * while the phone lies still. Both are offered with a plain reason and
 * never forced: without them the service still runs, it is only stopped
 * more often and its notifications are not seen.
 */
final class Asks {
    private static final String FILE = "asks";
    private static final String BATTERY = "battery";
    private static final String NOTIFICATIONS = "notifications";

    private Asks() {}

    static void once(Activity screen) {
        SharedPreferences asked = screen.getSharedPreferences(FILE, Activity.MODE_PRIVATE);
        if (Build.VERSION.SDK_INT >= 33 && !asked.getBoolean(NOTIFICATIONS, false)) {
            asked.edit().putBoolean(NOTIFICATIONS, true).apply();
            if (screen.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS)
                    != PackageManager.PERMISSION_GRANTED) {
                screen.requestPermissions(
                        new String[] {Manifest.permission.POST_NOTIFICATIONS}, 0);
                // One question at a time: the battery's comes on the next
                // look at the list.
                return;
            }
        }
        if (asked.getBoolean(BATTERY, false)) {
            return;
        }
        asked.edit().putBoolean(BATTERY, true).apply();
        PowerManager power = screen.getSystemService(PowerManager.class);
        if (power.isIgnoringBatteryOptimizations(screen.getPackageName())) {
            return;
        }
        battery(screen);
    }

    /** The first start asked both itself (WelcomeActivity): the list asks neither again. */
    static void asked(Activity screen) {
        screen.getSharedPreferences(FILE, Activity.MODE_PRIVATE).edit()
                .putBoolean(NOTIFICATIONS, true)
                .putBoolean(BATTERY, true)
                .apply();
    }

    private static void battery(final Activity screen) {
        new AlertDialog.Builder(screen)
                .setTitle(R.string.battery_title)
                .setMessage(R.string.battery_text)
                .setPositiveButton(R.string.battery_yes, new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface dialog, int which) {
                        Intent ask = new Intent(
                                Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS,
                                Uri.parse("package:" + screen.getPackageName()));
                        try {
                            screen.startActivity(ask);
                        } catch (RuntimeException none) {
                            // A phone without the dialog: nothing to ask.
                        }
                    }
                })
                .setNegativeButton(R.string.battery_no, null)
                .show();
    }
}

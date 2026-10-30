package network.leviculum.loquax;

import android.app.Activity;
import android.content.ComponentName;
import android.content.Intent;
import android.content.ServiceConnection;
import android.os.IBinder;

/**
 * A screen's binding to {@link LoquaxService} while it is visible: bound in
 * onStart, let go in onStop. The binding never creates the service; it
 * tells a running one that a screen is in front, so it posts no message
 * notification over the app.
 */
final class ServiceLink implements ServiceConnection {
    private final Activity screen;
    private LoquaxService service;
    private boolean bound;

    ServiceLink(Activity screen) {
        this.screen = screen;
    }

    void bind() {
        bound = screen.bindService(new Intent(screen, LoquaxService.class), this, 0);
    }

    void unbind() {
        if (service != null) {
            service.screen(false);
            service = null;
        }
        if (bound) {
            screen.unbindService(this);
            bound = false;
        }
    }

    @Override
    public void onServiceConnected(ComponentName name, IBinder binder) {
        service = ((LoquaxService.Local) binder).service();
        service.screen(true);
    }

    @Override
    public void onServiceDisconnected(ComponentName name) {
        service = null;
    }
}

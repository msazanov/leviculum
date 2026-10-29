package network.leviculum.loquax;

import android.app.Activity;
import android.os.Bundle;
import android.widget.TextView;

/** The first screen: what the Rust library says about this device. */
public class MainActivity extends Activity {
    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.activity_main);
        TextView status = (TextView) findViewById(R.id.status);
        // The app's files directory is its loquax home: the place
        // `home-client` and `home-cursor` live, as $LNMSG_HOME on a desktop.
        status.setText(Native.describe(getFilesDir().getAbsolutePath()));
    }
}

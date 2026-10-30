package network.leviculum.loquax;

import android.app.Activity;
import android.os.Bundle;
import android.view.View;
import android.widget.Button;
import android.widget.FrameLayout;
import android.widget.TextView;
import android.widget.Toast;
import java.util.Map;

/**
 * The map: the contacts' latest positions as pins over loquax's own map,
 * drawn by the library ({@link MapView}). Opened from the list on every
 * pin, or from a message that carries a position, on its fix with its
 * sender's pin selected. A tap on a pin names it; the
 * status line carries the map's credit and what a load said.
 */
public class MapActivity extends Activity implements Events.Listener, MapView.Listener {
    static final String LAT_E6 = "lat_e6";
    static final String LON_E6 = "lon_e6";
    /** The pin to select where the map opens on a fix: its sender's address. */
    static final String SELECTED = "selected";

    private TextView status;
    private TextView credit;
    private Button theme;
    private MapView map;
    private final ServiceLink link = new ServiceLink(this);

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.activity_map);
        status = (TextView) findViewById(R.id.status);
        credit = (TextView) findViewById(R.id.credit);
        map = new MapView(this, getFilesDir().getAbsolutePath(), this);
        if (getIntent().hasExtra(LAT_E6) && getIntent().hasExtra(LON_E6)) {
            map.centreOn(getIntent().getLongExtra(LAT_E6, 0), getIntent().getLongExtra(LON_E6, 0),
                    getIntent().getStringExtra(SELECTED));
        }
        ((FrameLayout) findViewById(R.id.map)).addView(map);
        theme = (Button) findViewById(R.id.theme);
        theme.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                map.toggleTheme();
            }
        });
        theme.setOnLongClickListener(new View.OnLongClickListener() {
            @Override
            public boolean onLongClick(View v) {
                map.followPhone();
                Toast.makeText(MapActivity.this, R.string.map_follows_phone, Toast.LENGTH_SHORT).show();
                return true;
            }
        });
        ((Button) findViewById(R.id.fit)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                map.fitPins();
            }
        });
        ((Button) findViewById(R.id.load)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                status.setText(R.string.map_loading);
                map.loadArea();
            }
        });
    }

    @Override
    protected void onStart() {
        super.onStart();
        link.bind();
    }

    @Override
    protected void onStop() {
        link.unbind();
        super.onStop();
    }

    @Override
    protected void onResume() {
        super.onResume();
        Events.listen(this);
        Core.front(true);
        map.ask();
    }

    @Override
    protected void onPause() {
        Core.front(false);
        Events.listen(null);
        super.onPause();
    }

    @Override
    public void onShown(Map<String, Object> shown) {
        if (!Core.bool(shown, "ok")) {
            status.setText(Core.str(shown, "error"));
            return;
        }
        credit.setText(Core.str(shown, "attribution"));
        // The table the other press gives: a sun on a dark map, a moon on
        // a light one.
        theme.setText(Core.bool(shown, "dark") ? "\u2600" : "\u263E");
        if (shown.get("tapped_name") != null) {
            status.setText(Core.str(shown, "tapped_name"));
        } else if (shown.get("notice") != null) {
            status.setText(Core.str(shown, "notice"));
        } else if (Core.bool(shown, "loading")) {
            status.setText(R.string.map_loading);
        } else if (Core.num(shown, "pins") == 0) {
            status.setText(R.string.map_no_pins);
        } else {
            status.setText("");
        }
    }

    @Override
    public void onEvent(Map<String, Object> event) {
        // A position that came moves its pin: the view is drawn anew.
        if ("changed".equals(Core.str(event, "t"))) {
            map.ask();
            return;
        }
        String line = Ui.eventLine(this, event);
        if (line != null) {
            status.setText(line);
        }
    }
}

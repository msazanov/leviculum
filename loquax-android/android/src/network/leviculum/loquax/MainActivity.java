package network.leviculum.loquax;

import android.app.Activity;
import android.content.Intent;
import android.os.Build;
import android.os.Bundle;
import android.view.View;
import android.widget.Button;
import android.widget.EditText;
import android.widget.TextView;
import java.util.Map;

/**
 * The first screen: starts the session, and on a phone that is not joined
 * yet asks for the home code and the home node's address. Once started it
 * hands over to the conversation list.
 */
public class MainActivity extends Activity {
    private TextView status;
    private TextView error;
    private View form;
    private EditText code;
    private EditText server;
    private Button join;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.activity_main);
        status = (TextView) findViewById(R.id.status);
        error = (TextView) findViewById(R.id.error);
        form = findViewById(R.id.form);
        code = (EditText) findViewById(R.id.code);
        server = (EditText) findViewById(R.id.server);
        join = (Button) findViewById(R.id.join);
        // The app's files directory is its loquax home: the place
        // `home-client` and `home-cursor` live, as $LNMSG_HOME on a desktop.
        status.setText(Native.describe(home()));
        form.setVisibility(View.GONE);
        join.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                start(true);
            }
        });
        start(false);
    }

    private String home() {
        return getFilesDir().getAbsolutePath();
    }

    /** Start the session, with what the form holds when asked to join. */
    private void start(final boolean joining) {
        final String typedCode = joining ? code.getText().toString().trim() : null;
        final String typedServer = joining ? server.getText().toString().trim() : null;
        join.setEnabled(false);
        error.setText(joining ? getString(R.string.joining) : getString(R.string.starting));
        final String home = home();
        new Thread(new Runnable() {
            @Override
            public void run() {
                final Map<String, Object> answer = Core.start(
                        home,
                        typedServer == null || typedServer.isEmpty() ? null : typedServer,
                        typedCode == null || typedCode.isEmpty() ? null : typedCode,
                        Build.MODEL);
                runOnUiThread(new Runnable() {
                    @Override
                    public void run() {
                        started(answer);
                    }
                });
            }
        }, "loquax-start").start();
    }

    private void started(Map<String, Object> started) {
        join.setEnabled(true);
        if (Core.bool(started, "ok")) {
            Events.start();
            // The session outlives this screen from here on.
            LoquaxService.start(this);
            startActivity(new Intent(this, ConversationsActivity.class));
            finish();
            return;
        }
        form.setVisibility(View.VISIBLE);
        error.setText(Core.str(started, "error"));
        String need = Core.str(started, "need");
        if ("server".equals(need)) {
            server.requestFocus();
        } else if ("code".equals(need)) {
            code.requestFocus();
        }
    }
}

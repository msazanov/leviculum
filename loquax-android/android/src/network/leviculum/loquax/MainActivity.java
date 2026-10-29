package network.leviculum.loquax;

import android.app.Activity;
import android.content.Intent;
import android.os.Build;
import android.os.Bundle;
import android.text.Editable;
import android.text.TextWatcher;
import android.view.View;
import android.widget.Button;
import android.widget.EditText;
import android.widget.TextView;
import java.util.Map;

/**
 * The first screen: starts the session, and on a phone that is not joined
 * yet asks for the home code and the home node's address. Once started it
 * hands over to the conversation list.
 *
 * A code from {@code loquax home admit --server} carries the address
 * ({@code lqxh1...@host:port}): pasted, it fills the address field too;
 * shared from another app (a QR reader's share button), it fills both and
 * joins at once.
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
        code.addTextChangedListener(new TextWatcher() {
            @Override
            public void beforeTextChanged(CharSequence s, int start, int count, int after) {}

            @Override
            public void onTextChanged(CharSequence s, int start, int before, int count) {}

            @Override
            public void afterTextChanged(Editable s) {
                fillServer(s.toString());
            }
        });
        start(shared(getIntent()));
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        if (shared(intent)) {
            start(true);
        }
    }

    /**
     * A code shared to the app: into the form. True when it carries the
     * address, so joining needs nothing else.
     */
    private boolean shared(Intent intent) {
        if (intent == null || !Intent.ACTION_SEND.equals(intent.getAction())) {
            return false;
        }
        CharSequence text = intent.getCharSequenceExtra(Intent.EXTRA_TEXT);
        String found = text == null ? "" : homeCode(text.toString());
        if (found.isEmpty()) {
            return false;
        }
        code.setText(found);
        return found.indexOf('@') > 0;
    }

    /** The first word that starts like a home code, in either case; empty for none. */
    static String homeCode(String text) {
        for (String word : text.split("\\s+")) {
            if (word.regionMatches(true, 0, "lqxh1", 0, 5)) {
                return word;
            }
        }
        return "";
    }

    /** The address behind a combined code, into an empty address field. */
    private void fillServer(String text) {
        int at = text.indexOf('@');
        if (at < 0 || !server.getText().toString().trim().isEmpty()) {
            return;
        }
        String address = text.substring(at + 1).trim();
        if (!address.isEmpty()) {
            server.setText(address);
        }
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

package network.leviculum.loquax;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.DialogInterface;
import android.os.Bundle;
import android.view.Gravity;
import android.view.View;
import android.view.ViewGroup;
import android.widget.AdapterView;
import android.widget.BaseAdapter;
import android.widget.Button;
import android.widget.EditText;
import android.widget.LinearLayout;
import android.widget.ListView;
import android.widget.TextView;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;

/**
 * One conversation: bubbles, ours right and theirs left, each of ours with
 * its state from the home node's archive; reactions as chips under their
 * message; compose and send through the home node. Opening it reads the
 * conversation and pulls; leaving it closes it, so what arrives then stays
 * unread.
 */
public class ChatActivity extends Activity implements Events.Listener {
    static final String PEER = "peer";
    /** loquax's quick reactions (loquax/src/ui/reactions.rs, QUICK). */
    private static final String[] QUICK = {
        "👍", "❤️", "😂", "😮", "😢", "😡",
    };

    private final List<Map<String, Object>> rows = new ArrayList<>();
    private String peer;
    private TextView title;
    private TextView status;
    private EditText compose;
    private Adapter adapter;
    private final ServiceLink link = new ServiceLink(this);

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.activity_chat);
        peer = getIntent().getStringExtra(PEER);
        title = (TextView) findViewById(R.id.title);
        status = (TextView) findViewById(R.id.status);
        compose = (EditText) findViewById(R.id.compose);
        ListView list = (ListView) findViewById(R.id.list);
        adapter = new Adapter();
        list.setAdapter(adapter);
        list.setOnItemLongClickListener(new AdapterView.OnItemLongClickListener() {
            @Override
            public boolean onItemLongClick(AdapterView<?> parent, View view, int position, long id) {
                react(rows.get(position));
                return true;
            }
        });
        ((Button) findViewById(R.id.refresh)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                say(Core.pull());
            }
        });
        ((Button) findViewById(R.id.send)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                send();
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
        String error = Core.open(peer);
        if (Ui.NOT_STARTED.equals(error)) {
            finish();
            return;
        }
        Core.front(true);
        say(error);
        refresh();
    }

    @Override
    protected void onPause() {
        Core.front(false);
        Events.listen(null);
        Core.close();
        super.onPause();
    }

    private void say(String error) {
        if (error != null) {
            status.setText(error);
        }
    }

    private void send() {
        String body = compose.getText().toString();
        if (body.trim().isEmpty()) {
            return;
        }
        String error = Core.send(peer, body);
        if (error != null) {
            status.setText(error);
            return;
        }
        compose.setText("");
        refresh();
    }

    /** A message's reactions: one of loquax's quick six. */
    private void react(Map<String, Object> row) {
        final String target = Core.str(row, "id");
        if (target.isEmpty()) {
            status.setText(R.string.no_reaction_yet);
            return;
        }
        new AlertDialog.Builder(this)
                .setTitle(R.string.react)
                .setItems(QUICK, new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface dialog, int which) {
                        say(Core.react(peer, target, QUICK[which]));
                    }
                })
                .show();
    }

    private void refresh() {
        Map<String, Object> answer = Core.messages(peer);
        if (answer.containsKey("error")) {
            status.setText(Core.str(answer, "error"));
            return;
        }
        title.setText(Core.str(answer, "name"));
        rows.clear();
        rows.addAll(Core.list(answer, "messages"));
        adapter.notifyDataSetChanged();
    }

    @Override
    public void onEvent(Map<String, Object> event) {
        if ("changed".equals(Core.str(event, "t"))) {
            refresh();
            return;
        }
        if ("stopped".equals(Core.str(event, "t"))) {
            finish();
            return;
        }
        String line = Ui.eventLine(this, event);
        if (line != null) {
            status.setText(line);
        }
    }

    /** The state of one of ours, in words. */
    private String state(String state) {
        switch (state) {
            case "queued": return getString(R.string.state_queued);
            case "sending": return getString(R.string.state_sending);
            case "sent": return getString(R.string.state_sent);
            case "delivered": return getString(R.string.state_delivered);
            case "failed": return getString(R.string.state_failed);
            default: return state;
        }
    }

    private final class Adapter extends BaseAdapter {
        @Override
        public int getCount() {
            return rows.size();
        }

        @Override
        public Object getItem(int position) {
            return rows.get(position);
        }

        @Override
        public long getItemId(int position) {
            return position;
        }

        @Override
        public View getView(int position, View view, ViewGroup parent) {
            if (view == null) {
                view = getLayoutInflater().inflate(R.layout.row_message, parent, false);
            }
            Map<String, Object> row = rows.get(position);
            boolean ours = Core.bool(row, "ours");
            ((LinearLayout) view).setGravity(ours ? Gravity.END : Gravity.START);
            View bubble = view.findViewById(R.id.bubble);
            bubble.setBackgroundResource(ours ? R.drawable.bubble_ours : R.drawable.bubble_theirs);
            ((TextView) view.findViewById(R.id.body)).setText(Core.str(row, "body"));
            String meta = Ui.when(Core.num(row, "at"));
            if (ours) {
                meta = meta.isEmpty() ? state(Core.str(row, "state"))
                        : meta + "  " + state(Core.str(row, "state"));
            }
            ((TextView) view.findViewById(R.id.meta)).setText(meta);
            StringBuilder chips = new StringBuilder();
            for (Map<String, Object> chip : Core.list(row, "chips")) {
                if (chips.length() > 0) {
                    chips.append("  ");
                }
                String text = Core.str(chip, "content");
                long count = Core.num(chip, "count");
                if (count > 1) {
                    text = text + " " + count;
                }
                chips.append(Core.bool(chip, "ours") ? "[" + text + "]" : text);
            }
            TextView chipView = (TextView) view.findViewById(R.id.chips);
            chipView.setVisibility(chips.length() > 0 ? View.VISIBLE : View.GONE);
            chipView.setText(chips);
            return view;
        }
    }
}

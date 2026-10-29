package network.leviculum.loquax;

import android.app.Activity;
import android.content.Intent;
import android.os.Bundle;
import android.view.View;
import android.view.ViewGroup;
import android.widget.AdapterView;
import android.widget.BaseAdapter;
import android.widget.Button;
import android.widget.EditText;
import android.widget.ListView;
import android.widget.TextView;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;

/** The conversation list: names, last message, unread; most recent first. */
public class ConversationsActivity extends Activity implements Events.Listener {
    private final List<Map<String, Object>> rows = new ArrayList<>();
    private TextView status;
    private Adapter adapter;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.activity_conversations);
        status = (TextView) findViewById(R.id.status);
        ListView list = (ListView) findViewById(R.id.list);
        adapter = new Adapter();
        list.setAdapter(adapter);
        list.setEmptyView(findViewById(R.id.empty));
        list.setOnItemClickListener(new AdapterView.OnItemClickListener() {
            @Override
            public void onItemClick(AdapterView<?> parent, View view, int position, long id) {
                chat(Core.str(rows.get(position), "peer"));
            }
        });
        ((Button) findViewById(R.id.refresh)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                say(Core.pull());
            }
        });
        final EditText address = (EditText) findViewById(R.id.address);
        ((Button) findViewById(R.id.open)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                openTyped(address);
            }
        });
    }

    /** A new conversation, by the address typed below the list. */
    private void openTyped(EditText address) {
        String peer = address.getText().toString().trim();
        if (peer.isEmpty()) {
            return;
        }
        String error = Core.open(peer);
        if (error != null) {
            status.setText(error);
            return;
        }
        address.setText("");
        chat(peer);
    }

    private void chat(String peer) {
        startActivity(new Intent(this, ChatActivity.class).putExtra(ChatActivity.PEER, peer));
    }

    @Override
    protected void onResume() {
        super.onResume();
        Events.listen(this);
        Core.close();
        refresh();
    }

    @Override
    protected void onPause() {
        Events.listen(null);
        super.onPause();
    }

    private void say(String error) {
        if (error != null) {
            status.setText(error);
        }
    }

    private void refresh() {
        Map<String, Object> answer = Core.conversations();
        if (answer.containsKey("error")) {
            status.setText(Core.str(answer, "error"));
            return;
        }
        rows.clear();
        rows.addAll(Core.list(answer, "conversations"));
        adapter.notifyDataSetChanged();
    }

    @Override
    public void onEvent(Map<String, Object> event) {
        String kind = Core.str(event, "t");
        if ("changed".equals(kind)) {
            refresh();
        } else {
            String line = Ui.eventLine(this, event);
            if (line != null) {
                status.setText(line);
            }
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
                view = getLayoutInflater().inflate(R.layout.row_conversation, parent, false);
            }
            Map<String, Object> row = rows.get(position);
            ((TextView) view.findViewById(R.id.name)).setText(Core.str(row, "name"));
            String last = Core.str(row, "last");
            if (Core.bool(row, "ours") && !last.isEmpty()) {
                last = getString(R.string.you) + " " + last;
            }
            ((TextView) view.findViewById(R.id.last)).setText(last);
            ((TextView) view.findViewById(R.id.at)).setText(Ui.when(Core.num(row, "at")));
            long unread = Core.num(row, "unread");
            TextView badge = (TextView) view.findViewById(R.id.unread);
            badge.setVisibility(unread > 0 ? View.VISIBLE : View.GONE);
            badge.setText(String.valueOf(unread));
            return view;
        }
    }
}

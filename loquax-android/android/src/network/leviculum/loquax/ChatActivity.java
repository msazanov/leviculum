package network.leviculum.loquax;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.DialogInterface;
import android.content.Intent;
import android.graphics.drawable.GradientDrawable;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.text.Editable;
import android.text.SpannableStringBuilder;
import android.text.Spanned;
import android.text.TextWatcher;
import android.text.style.ForegroundColorSpan;
import android.text.style.RelativeSizeSpan;
import android.view.Gravity;
import android.view.MenuItem;
import android.view.View;
import android.view.ViewGroup;
import android.widget.AdapterView;
import android.widget.ArrayAdapter;
import android.widget.BaseAdapter;
import android.widget.EditText;
import android.widget.GridView;
import android.widget.ImageButton;
import android.widget.ImageView;
import android.widget.LinearLayout;
import android.widget.ListView;
import android.widget.PopupMenu;
import android.widget.TextView;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;

/**
 * One conversation, after a modern messenger's layout (1129): a bar with
 * the peer's avatar and name; bubbles, ours right and theirs left, grouped
 * when one sender writes several within a few minutes, each with its time
 * and for ours the delivery mark inside; day separators; reactions as pills
 * over a bubble's edge; a pill-shaped compose field with a round send
 * button. Opening it reads the conversation and pulls; leaving it closes
 * it, so what arrives then stays unread.
 */
public class ChatActivity extends Activity implements Events.Listener {
    static final String PEER = "peer";
    /** loquax's quick reactions (loquax/src/ui/reactions.rs, QUICK). */
    private static final String[] QUICK = {
        "👍", "❤️", "😂", "😮", "😢", "😡",
    };

    /** A few common emoji for the compose field's emoji button. */
    private static final String[] EMOJI = {
        "😀", "😂", "😊", "😍", "😘", "😉", "🤔", "😮",
        "😢", "😡", "👍", "👎", "👏", "🙏", "💪", "👋",
        "❤️", "🔥", "🎉", "✅", "❌", "📍", "☕", "🍺",
    };
    /** Messages of one sender this close together form a group. */
    private static final long GROUP_SECS = 180;
    /** The bubble's corner, and a grouped bubble's inner one, in dp. */
    private static final float CORNER = 18;
    private static final float CORNER_GROUPED = 6;

    private final List<Map<String, Object>> rows = new ArrayList<>();
    private String peer;
    private TextView title;
    private AvatarView avatar;
    private TextView status;
    private EditText compose;
    private ImageButton send;
    private Adapter adapter;
    private final ServiceLink link = new ServiceLink(this);
    /** Redraws the relative times of the newest group while the screen is up. */
    private final Handler ticker = new Handler(Looper.getMainLooper());
    private final Runnable tick = new Runnable() {
        @Override
        public void run() {
            adapter.notifyDataSetChanged();
            ticker.postDelayed(this, 30_000);
        }
    };

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.activity_chat);
        peer = getIntent().getStringExtra(PEER);
        title = (TextView) findViewById(R.id.title);
        avatar = (AvatarView) findViewById(R.id.avatar);
        status = (TextView) findViewById(R.id.status);
        compose = (EditText) findViewById(R.id.compose);
        send = (ImageButton) findViewById(R.id.send);
        int text = getColor(R.color.text);
        ImageButton back = (ImageButton) findViewById(R.id.back);
        back.setImageDrawable(new Glyph(Glyph.Kind.BACK, text));
        back.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                finish();
            }
        });
        final ImageButton menu = (ImageButton) findViewById(R.id.menu);
        menu.setImageDrawable(new Glyph(Glyph.Kind.MENU, text));
        menu.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                showMenu(menu);
            }
        });
        ImageButton emoji = (ImageButton) findViewById(R.id.emoji);
        emoji.setImageDrawable(new Glyph(Glyph.Kind.EMOJI, getColor(R.color.text_secondary)));
        emoji.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                pickEmoji();
            }
        });
        send.setImageDrawable(new Glyph(Glyph.Kind.SEND, getColor(R.color.on_send)));
        compose.addTextChangedListener(new TextWatcher() {
            @Override
            public void beforeTextChanged(CharSequence s, int start, int count, int after) {}

            @Override
            public void onTextChanged(CharSequence s, int start, int before, int count) {}

            @Override
            public void afterTextChanged(Editable s) {
                sendable();
            }
        });
        sendable();
        ListView list = (ListView) findViewById(R.id.list);
        adapter = new Adapter();
        list.setAdapter(adapter);
        list.setOnItemClickListener(new AdapterView.OnItemClickListener() {
            @Override
            public void onItemClick(AdapterView<?> parent, View view, int position, long id) {
                showFix(rows.get(position));
            }
        });
        list.setOnItemLongClickListener(new AdapterView.OnItemLongClickListener() {
            @Override
            public boolean onItemLongClick(AdapterView<?> parent, View view, int position, long id) {
                react(rows.get(position));
                return true;
            }
        });
        send.setOnClickListener(new View.OnClickListener() {
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
        ticker.postDelayed(tick, 30_000);
    }

    @Override
    protected void onPause() {
        ticker.removeCallbacks(tick);
        Core.front(false);
        Events.listen(null);
        Core.close();
        super.onPause();
    }

    private void say(String error) {
        if (error != null) {
            line(error);
        }
    }

    /** The line under the bar, shown only while it has something to say. */
    private void line(String text) {
        status.setText(text);
        status.setVisibility(text.isEmpty() ? View.GONE : View.VISIBLE);
    }

    /** The send button is bright with text to send and dimmed without. */
    private void sendable() {
        boolean some = !compose.getText().toString().trim().isEmpty();
        send.setEnabled(some);
        send.setAlpha(some ? 1f : 0.38f);
    }

    /** The bar's menu: pull now, the map. */
    private void showMenu(View anchor) {
        PopupMenu popup = new PopupMenu(this, anchor);
        popup.getMenu().add(0, R.string.refresh, 0, R.string.refresh);
        popup.getMenu().add(0, R.string.map, 1, R.string.map);
        popup.setOnMenuItemClickListener(new PopupMenu.OnMenuItemClickListener() {
            @Override
            public boolean onMenuItemClick(MenuItem item) {
                if (item.getItemId() == R.string.refresh) {
                    say(Core.pull());
                } else {
                    startActivity(new Intent(ChatActivity.this, MapActivity.class));
                }
                return true;
            }
        });
        popup.show();
    }

    /** One of a few emoji into the compose field, at the cursor. */
    private void pickEmoji() {
        GridView grid = new GridView(this);
        grid.setNumColumns(6);
        int pad = dp(8);
        grid.setPadding(pad, pad, pad, pad);
        grid.setAdapter(new ArrayAdapter<String>(this, android.R.layout.simple_list_item_1, EMOJI) {
            @Override
            public View getView(int position, View view, ViewGroup parent) {
                TextView cell = (TextView) super.getView(position, view, parent);
                cell.setGravity(Gravity.CENTER);
                cell.setTextSize(26);
                return cell;
            }
        });
        final AlertDialog dialog = new AlertDialog.Builder(this).setView(grid).create();
        grid.setOnItemClickListener(new AdapterView.OnItemClickListener() {
            @Override
            public void onItemClick(AdapterView<?> parent, View view, int position, long id) {
                int at = Math.max(compose.getSelectionStart(), 0);
                int end = Math.max(compose.getSelectionEnd(), 0);
                compose.getText().replace(Math.min(at, end), Math.max(at, end), EMOJI[position]);
                dialog.dismiss();
            }
        });
        dialog.show();
    }

    private int dp(float dp) {
        return Math.round(dp * getResources().getDisplayMetrics().density);
    }

    private void send() {
        String body = compose.getText().toString();
        if (body.trim().isEmpty()) {
            return;
        }
        String error = Core.send(peer, body);
        if (error != null) {
            line(error);
            return;
        }
        compose.setText("");
        refresh();
    }

    /**
     * A message that carries a position, a report or a text, opens the map
     * centred on its fix with the sender's pin selected, as Enter does in
     * the terminal. Back comes here again where the list was.
     */
    private void showFix(Map<String, Object> row) {
        if (row.get("lat_e6") == null || row.get("lon_e6") == null) {
            return;
        }
        startActivity(new Intent(this, MapActivity.class)
                .putExtra(MapActivity.LAT_E6, Core.num(row, "lat_e6"))
                .putExtra(MapActivity.LON_E6, Core.num(row, "lon_e6"))
                .putExtra(MapActivity.SELECTED, peer));
    }

    /**
     * The bubble's text: the body, and under it, for a message that carries
     * a position, its line (1132): a pin, the coordinates, the accuracy
     * when the sender gave one, how old the fix is; dimmed once it is
     * stale. A report has no body: its line is the bubble.
     */
    private CharSequence text(Map<String, Object> row, boolean ours, long now) {
        String body = Core.str(row, "body");
        if (row.get("place") == null) {
            return body;
        }
        StringBuilder line = new StringBuilder("\uD83D\uDCCD ").append(Core.str(row, "place"));
        String accuracy = Core.str(row, "accuracy");
        if (!accuracy.isEmpty()) {
            line.append(" \u00B7 ").append(accuracy);
        }
        if (row.get("fix_at") != null) {
            line.append(" \u00B7 ").append(Ui.age(this, Core.num(row, "fix_at"), now));
        }
        SpannableStringBuilder out = new SpannableStringBuilder(body);
        if (out.length() > 0) {
            out.append('\n');
        }
        int start = out.length();
        out.append(line);
        int end = out.length();
        out.setSpan(new RelativeSizeSpan(0.88f), start, end, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE);
        boolean stale = row.get("stale_at") != null && now > Core.num(row, "stale_at");
        if (stale) {
            int dim = getColor(ours ? R.color.bubble_ours_meta : R.color.bubble_theirs_meta);
            out.setSpan(new ForegroundColorSpan(dim), start, end, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE);
        }
        return out;
    }

    /** A message's reactions: one of loquax's quick six. */
    private void react(Map<String, Object> row) {
        final String target = Core.str(row, "id");
        if (target.isEmpty()) {
            line(getString(R.string.no_reaction_yet));
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
            line(Core.str(answer, "error"));
            return;
        }
        title.setText(Core.str(answer, "name"));
        avatar.show(peer, answer);
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
        String words = Ui.eventLine(this, event);
        if (words != null) {
            line(words);
        }
    }

    /** The mark of one of ours, by its state. */
    private static Glyph.Kind mark(String state) {
        switch (state) {
            case "sent": return Glyph.Kind.SENT;
            case "delivered": return Glyph.Kind.DELIVERED;
            case "failed": return Glyph.Kind.FAILED;
            default: return Glyph.Kind.QUEUED;
        }
    }

    /** Whether row `i` continues the group of row `i - 1`: one sender, one day, a few minutes. */
    private boolean joinsPrevious(int i) {
        if (i <= 0 || i >= rows.size()) {
            return false;
        }
        Map<String, Object> a = rows.get(i - 1);
        Map<String, Object> b = rows.get(i);
        long at = Core.num(a, "at");
        long bt = Core.num(b, "at");
        return Core.bool(a, "ours") == Core.bool(b, "ours")
                && at > 0 && bt > 0
                && Math.abs(bt - at) <= GROUP_SECS
                && Ui.daysBetween(at, bt) == 0;
    }

    /** Whether a new day begins at row `i`. */
    private boolean dayBegins(int i) {
        long at = Core.num(rows.get(i), "at");
        if (at <= 0) {
            return false;
        }
        if (i == 0) {
            return true;
        }
        long before = Core.num(rows.get(i - 1), "at");
        return before <= 0 || Ui.daysBetween(before, at) != 0;
    }

    /** Whether row `i` is in the conversation's newest group. */
    private boolean newest(int i) {
        for (int j = i + 1; j < rows.size(); j++) {
            if (!joinsPrevious(j)) {
                return false;
            }
        }
        return true;
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
            boolean joinsBefore = joinsPrevious(position);
            boolean joinsAfter = joinsPrevious(position + 1);
            long at = Core.num(row, "at");
            long now = Ui.nowSecs();
            LinearLayout root = (LinearLayout) view;
            root.setGravity(ours ? Gravity.END : Gravity.START);

            TextView day = (TextView) view.findViewById(R.id.day);
            boolean begins = dayBegins(position);
            day.setVisibility(begins ? View.VISIBLE : View.GONE);
            if (begins) {
                day.setText(Ui.day(ChatActivity.this, at, now));
            }
            root.setPadding(root.getPaddingStart(), joinsBefore ? dp(2) : dp(begins ? 0 : 10),
                    root.getPaddingEnd(), 0);

            BubbleLayout bubble = (BubbleLayout) view.findViewById(R.id.bubble);
            int width = parent.getWidth() > 0
                    ? parent.getWidth() : getResources().getDisplayMetrics().widthPixels;
            bubble.setMaxWidth(Math.round(width * 0.8f));
            float big = dp(CORNER);
            float small = dp(CORNER_GROUPED);
            float top = joinsBefore ? small : big;
            float bottom = joinsAfter ? small : big;
            GradientDrawable shape = new GradientDrawable();
            shape.setColor(getColor(ours ? R.color.bubble_ours : R.color.bubble_theirs));
            // Top left, top right, bottom right, bottom left; the inner
            // side is the sender's: the right for ours.
            shape.setCornerRadii(ours
                    ? new float[] {big, big, top, top, bottom, bottom, big, big}
                    : new float[] {top, top, big, big, big, big, bottom, bottom});
            bubble.setBackground(shape);

            TextView body = (TextView) view.findViewById(R.id.body);
            body.setText(text(row, ours, now));
            body.setTextColor(getColor(ours ? R.color.bubble_ours_text : R.color.bubble_theirs_text));

            // Only a group's last shows its time, and one of ours whose
            // state differs from the next one's keeps its mark.
            String state = Core.str(row, "state");
            boolean showMeta = !joinsAfter || ours
                    && !state.equals(Core.str(rows.get(position + 1), "state"));
            View meta = view.findViewById(R.id.meta);
            meta.setVisibility(showMeta && (at > 0 || ours) ? View.VISIBLE : View.GONE);
            int metaColor = getColor(ours ? R.color.bubble_ours_meta : R.color.bubble_theirs_meta);
            TextView time = (TextView) view.findViewById(R.id.time);
            String ago = newest(position) ? Ui.ago(ChatActivity.this, at, now) : null;
            time.setText(at <= 0 ? "" : ago != null ? ago : Ui.clock(at));
            time.setVisibility(at > 0 ? View.VISIBLE : View.GONE);
            time.setTextColor(metaColor);
            ImageView mark = (ImageView) view.findViewById(R.id.mark);
            mark.setVisibility(ours ? View.VISIBLE : View.GONE);
            if (ours) {
                mark.setImageDrawable(new Glyph(mark(state), metaColor, getColor(R.color.bubble_ours),
                        getColor(R.color.failed_ground), getColor(R.color.failed_mark)));
                mark.setContentDescription(state(state));
            }

            LinearLayout chips = (LinearLayout) view.findViewById(R.id.chips);
            chips.removeAllViews();
            for (Map<String, Object> chip : Core.list(row, "chips")) {
                chips.addView(pill(chip));
            }
            chips.setVisibility(chips.getChildCount() > 0 ? View.VISIBLE : View.GONE);
            return view;
        }

        /** A reaction: its emoji, a count above one, ours in our bubble's colour. */
        private TextView pill(Map<String, Object> chip) {
            boolean ours = Core.bool(chip, "ours");
            TextView pill = new TextView(ChatActivity.this);
            String text = Core.str(chip, "content");
            long count = Core.num(chip, "count");
            pill.setText(count > 1 ? text + " " + count : text);
            pill.setTextSize(14);
            pill.setTextColor(getColor(ours ? R.color.bubble_ours_text : R.color.bubble_theirs_text));
            pill.setPadding(dp(8), dp(2), dp(8), dp(2));
            GradientDrawable shape = new GradientDrawable();
            shape.setCornerRadius(dp(14));
            shape.setColor(getColor(ours ? R.color.bubble_ours : R.color.bubble_theirs));
            // A ring in the screen's colour lifts the pill off the bubble.
            shape.setStroke(dp(2), getColor(R.color.background));
            pill.setBackground(shape);
            LinearLayout.LayoutParams params = new LinearLayout.LayoutParams(
                    ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT);
            params.setMarginEnd(dp(2));
            pill.setLayoutParams(params);
            return pill;
        }
    }
}

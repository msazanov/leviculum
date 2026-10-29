package network.leviculum.loquax;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.DialogInterface;
import android.content.Intent;
import android.content.SharedPreferences;
import android.graphics.Typeface;
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
import android.os.Build;
import android.view.Gravity;
import android.view.MenuItem;
import android.view.View;
import android.view.ViewGroup;
import android.widget.AdapterView;
import android.widget.BaseAdapter;
import android.widget.Button;
import android.widget.EditText;
import android.widget.ImageButton;
import android.widget.ImageView;
import android.widget.LinearLayout;
import android.widget.ListView;
import android.widget.PopupMenu;
import android.widget.ScrollView;
import android.widget.TextView;
import java.util.ArrayList;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;

/**
 * One conversation, after a modern messenger's layout (1129): a bar with
 * the peer's avatar and name; bubbles, ours right and theirs left, grouped
 * when one sender writes several within a few minutes, each with its time
 * and for ours the delivery mark inside; day separators; reactions as pills
 * over a bubble's edge; a pill-shaped compose field with a round send
 * button. Opening it reads the conversation and pulls; leaving it closes
 * it, so what arrives then stays unread.
 *
 * A long press on a bubble (1137) opens Signal's menu (LongPress): the
 * quick reactions with the full picker behind "…", and reply, forward,
 * copy, select, info, pin and delete. What goes on the wire is Rust's
 * (loquax::ui::replies); a pin is this phone's alone.
 */
public class ChatActivity extends Activity implements Events.Listener {
    static final String PEER = "peer";
    /**
     * The long press's quick reactions: Signal's default six, the same
     * strings as loquax/src/ui/reactions.rs's SIGNAL_QUICK, U+FE0F and all.
     */
    private static final String[] QUICK = {
        "\u2764\uFE0F", "\uD83D\uDC4D", "\uD83D\uDC4E", "\uD83D\uDE02", "\uD83D\uDE2E", "\uD83D\uDE22",
    };
    /** The pins, by peer: the key of the pinned message, this phone only. */
    private static final String PINS = "pins";
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
    private ListView list;
    private boolean homeClient;
    /** The message a reply being written answers, or null. */
    private Map<String, Object> replyTo;
    private View replyBar;
    /** The selection's keys while selecting, else null. */
    private Set<Long> selection;
    private View selectionBar;
    private TextView selectionCount;
    private TextView pinned;
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
        replyBar = findViewById(R.id.reply_bar);
        ImageButton cancel = (ImageButton) findViewById(R.id.reply_cancel);
        cancel.setImageDrawable(new Glyph(Glyph.Kind.CLOSE, getColor(R.color.text_secondary)));
        cancel.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                replyTo(null);
            }
        });
        selectionBar = findViewById(R.id.selection_bar);
        selectionCount = (TextView) findViewById(R.id.selection_count);
        ImageButton close = (ImageButton) findViewById(R.id.selection_close);
        close.setImageDrawable(new Glyph(Glyph.Kind.CLOSE, text));
        close.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                select(null);
            }
        });
        ((Button) findViewById(R.id.selection_copy)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                copy(selected());
                select(null);
            }
        });
        ((Button) findViewById(R.id.selection_delete)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                askDelete(selected());
            }
        });
        pinned = (TextView) findViewById(R.id.pinned);
        pinned.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                goToPinned();
            }
        });
        pinned.setOnLongClickListener(new View.OnLongClickListener() {
            @Override
            public boolean onLongClick(View v) {
                pin(0);
                return true;
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
        list = (ListView) findViewById(R.id.list);
        adapter = new Adapter();
        list.setAdapter(adapter);
        list.setOnItemClickListener(new AdapterView.OnItemClickListener() {
            @Override
            public void onItemClick(AdapterView<?> parent, View view, int position, long id) {
                if (selection != null) {
                    toggle(rows.get(position));
                } else {
                    showFix(rows.get(position));
                }
            }
        });
        list.setOnItemLongClickListener(new AdapterView.OnItemLongClickListener() {
            @Override
            public boolean onItemLongClick(AdapterView<?> parent, View view, int position, long id) {
                if (selection != null) {
                    toggle(rows.get(position));
                } else {
                    longPress(view, rows.get(position));
                }
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

    /** Back ends a selection first, then a reply being written, then the screen. */
    @Override
    public void onBackPressed() {
        if (selection != null) {
            select(null);
        } else if (replyTo != null) {
            replyTo(null);
        } else {
            super.onBackPressed();
        }
    }

    private void say(String error) {
        if (error != null) {
            line(error);
        }
    }

    /** The line under the bar, shown only while it has something to say. */
    private void line(String text) {
        Ui.showLine(status, text, true);
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

    /** An emoji from the full picker into the compose field, at the cursor. */
    private void pickEmoji() {
        EmojiPicker.show(this, new EmojiPicker.Listener() {
            @Override
            public void picked(String emoji) {
                int at = Math.max(compose.getSelectionStart(), 0);
                int end = Math.max(compose.getSelectionEnd(), 0);
                compose.getText().replace(Math.min(at, end), Math.max(at, end), emoji);
                say(Core.used(emoji));
            }
        });
    }

    private int dp(float dp) {
        return Math.round(dp * getResources().getDisplayMetrics().density);
    }

    private void send() {
        String body = compose.getText().toString();
        if (body.trim().isEmpty()) {
            return;
        }
        String error = replyTo == null
                ? Core.send(peer, body) : Core.reply(peer, Core.str(replyTo, "id"), body);
        if (error != null) {
            line(error);
            return;
        }
        compose.setText("");
        replyTo(null);
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

    /** Our reaction on `row`, or null: the chip that is ours. */
    private static String ourReaction(Map<String, Object> row) {
        for (Map<String, Object> chip : Core.list(row, "chips")) {
            if (Core.bool(chip, "ours")) {
                return Core.str(chip, "content");
            }
        }
        return null;
    }

    /** The long press on the bubble of `row`, in the row's `view`. */
    private void longPress(View view, final Map<String, Object> row) {
        final boolean hasId = !Core.str(row, "id").isEmpty();
        boolean isPinned = pinnedKey() == Core.num(row, "key");
        int[] actions = {
            R.string.action_reply, R.string.action_forward, R.string.action_copy,
            R.string.action_select, R.string.action_info,
            isPinned ? R.string.action_unpin : R.string.action_pin, R.string.action_delete,
        };
        LongPress.show(this, view.findViewById(R.id.bubble), Core.bool(row, "ours"), hasId,
                QUICK, ourReaction(row), actions, new LongPress.Listener() {
                    @Override
                    public void react(String emoji) {
                        say(Core.react(peer, Core.str(row, "id"), emoji));
                    }

                    @Override
                    public void action(int action) {
                        act(action, row);
                    }
                });
    }

    private void act(int action, Map<String, Object> row) {
        List<Map<String, Object>> one = new ArrayList<>();
        one.add(row);
        if (action == R.string.action_reply) {
            if (Core.str(row, "id").isEmpty()) {
                line(getString(R.string.no_id_yet));
            } else {
                replyTo(row);
            }
        } else if (action == R.string.action_forward) {
            forward(row);
        } else if (action == R.string.action_copy) {
            copy(one);
        } else if (action == R.string.action_select) {
            select(new LinkedHashSet<Long>());
            toggle(row);
        } else if (action == R.string.action_info) {
            info(row);
        } else if (action == R.string.action_pin) {
            pin(Core.num(row, "key"));
        } else if (action == R.string.action_unpin) {
            pin(0);
        } else if (action == R.string.action_delete) {
            askDelete(one);
        }
    }

    /** Write a reply to `row`, its quote above the field; null ends it. */
    private void replyTo(Map<String, Object> row) {
        replyTo = row;
        replyBar.setVisibility(row == null ? View.GONE : View.VISIBLE);
        if (row == null) {
            return;
        }
        ((TextView) findViewById(R.id.reply_who)).setText(Core.bool(row, "ours")
                ? getString(R.string.reply_to_ours)
                : getString(R.string.reply_to_theirs, title.getText()));
        ((TextView) findViewById(R.id.reply_text)).setText(Core.str(row, "body"));
        compose.requestFocus();
    }

    /** Pick a conversation and send it a copy of `row`. */
    private void forward(final Map<String, Object> row) {
        final List<Map<String, Object>> all = Core.list(Core.conversations(), "conversations");
        String[] names = new String[all.size()];
        for (int i = 0; i < all.size(); i++) {
            names[i] = Core.str(all.get(i), "name");
        }
        new AlertDialog.Builder(this)
                .setTitle(R.string.forward_to)
                .setItems(names, new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface dialog, int which) {
                        Map<String, Object> to = all.get(which);
                        String error = Core.forward(peer, Core.num(row, "key"), Core.str(to, "peer"));
                        line(error != null ? error : getString(R.string.forwarded, Core.str(to, "name")));
                    }
                })
                .show();
    }

    /** The texts of `picked` to the clipboard, one after the other. */
    private void copy(List<Map<String, Object>> picked) {
        StringBuilder text = new StringBuilder();
        for (Map<String, Object> row : picked) {
            if (text.length() > 0) {
                text.append("\n\n");
            }
            text.append(Core.str(row, "body"));
        }
        ClipboardManager clipboard = (ClipboardManager) getSystemService(CLIPBOARD_SERVICE);
        if (clipboard == null) {
            return;
        }
        clipboard.setPrimaryClip(ClipData.newPlainText(getString(R.string.app_name), text));
        // Android 13 and later say so themselves.
        if (Build.VERSION.SDK_INT < 33) {
            line(getString(R.string.copied));
        }
    }

    /** Start selecting with `keys`, or end it with null. */
    private void select(Set<Long> keys) {
        selection = keys;
        selectionBar.setVisibility(keys == null ? View.GONE : View.VISIBLE);
        adapter.notifyDataSetChanged();
    }

    private void toggle(Map<String, Object> row) {
        Long key = Core.num(row, "key");
        if (!selection.remove(key)) {
            selection.add(key);
        }
        if (selection.isEmpty()) {
            select(null);
            return;
        }
        selectionCount.setText(getString(R.string.selected, selection.size()));
        adapter.notifyDataSetChanged();
    }

    /** The selected rows, in the conversation's order. */
    private List<Map<String, Object>> selected() {
        List<Map<String, Object>> picked = new ArrayList<>();
        for (Map<String, Object> row : rows) {
            if (selection != null && selection.contains(Core.num(row, "key"))) {
                picked.add(row);
            }
        }
        return picked;
    }

    /** Ask, then delete `picked`: from every device of the home on a client of one. */
    private void askDelete(final List<Map<String, Object>> picked) {
        if (picked.isEmpty()) {
            return;
        }
        int n = picked.size();
        String question = n == 1
                ? getString(homeClient ? R.string.delete_one_home : R.string.delete_one_here)
                : getString(homeClient ? R.string.delete_many_home : R.string.delete_many_here, n);
        new AlertDialog.Builder(this)
                .setMessage(question)
                .setPositiveButton(R.string.delete, new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface dialog, int which) {
                        List<Long> keys = new ArrayList<>();
                        for (Map<String, Object> row : picked) {
                            keys.add(Core.num(row, "key"));
                        }
                        say(Core.delete(peer, keys));
                        if (selection != null) {
                            select(null);
                        }
                        refresh();
                    }
                })
                .setNegativeButton(R.string.keep, null)
                .show();
    }

    /** The message's detail, the terminal's `v` view, in a dialog. */
    private void info(Map<String, Object> row) {
        String[] error = new String[1];
        List<String> lines = Core.info(peer, Core.num(row, "key"), error);
        if (lines == null) {
            line(error[0]);
            return;
        }
        StringBuilder text = new StringBuilder();
        for (String l : lines) {
            text.append(l).append('\n');
        }
        TextView view = new TextView(this);
        view.setText(text.toString().trim());
        view.setTypeface(Typeface.MONOSPACE);
        view.setTextSize(13);
        view.setTextIsSelectable(true);
        view.setTextColor(getColor(R.color.text));
        view.setPadding(dp(20), dp(12), dp(20), dp(12));
        ScrollView scroll = new ScrollView(this);
        scroll.addView(view);
        new AlertDialog.Builder(this)
                .setTitle(R.string.info)
                .setView(scroll)
                .setPositiveButton(android.R.string.ok, null)
                .show();
    }

    private SharedPreferences pins() {
        return getSharedPreferences(PINS, MODE_PRIVATE);
    }

    /** The key of this conversation's pinned message, 0 for none. */
    private long pinnedKey() {
        return pins().getLong(peer, 0);
    }

    /** Pin the message `key`, or none with 0; one per conversation. */
    private void pin(long key) {
        if (key == 0) {
            pins().edit().remove(peer).apply();
        } else {
            pins().edit().putLong(peer, key).apply();
        }
        showPinned();
    }

    /** The pinned bar: the pinned message's text, while it is still here. */
    private void showPinned() {
        long key = pinnedKey();
        Map<String, Object> row = null;
        for (Map<String, Object> r : rows) {
            if (key != 0 && Core.num(r, "key") == key) {
                row = r;
            }
        }
        pinned.setVisibility(row == null ? View.GONE : View.VISIBLE);
        if (row != null) {
            String body = Core.str(row, "body");
            pinned.setText("\uD83D\uDCCC " + (body.isEmpty() ? Core.str(row, "place") : body));
            pinned.setContentDescription(getString(R.string.pinned) + ": " + body);
        }
    }

    private void goToPinned() {
        long key = pinnedKey();
        for (int i = 0; i < rows.size(); i++) {
            if (Core.num(rows.get(i), "key") == key) {
                list.setSelection(i);
                return;
            }
        }
    }

    private void refresh() {
        Map<String, Object> answer = Core.messages(peer);
        if (answer.containsKey("error")) {
            line(Core.str(answer, "error"));
            return;
        }
        title.setText(Core.str(answer, "name"));
        avatar.show(peer, answer);
        homeClient = Core.bool(answer, "home_client");
        rows.clear();
        rows.addAll(Core.list(answer, "messages"));
        if (selection != null) {
            // What another device deleted is no longer selected.
            Set<Long> here = new LinkedHashSet<>();
            for (Map<String, Object> row : rows) {
                here.add(Core.num(row, "key"));
            }
            selection.retainAll(here);
            if (selection.isEmpty()) {
                select(null);
            } else {
                selectionCount.setText(getString(R.string.selected, selection.size()));
            }
        }
        showPinned();
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
            Ui.showLine(status, words, Ui.stays(event));
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
            boolean picked = selection != null && selection.contains(Core.num(row, "key"));
            root.setBackgroundColor(picked ? getColor(R.color.selected_row) : 0);

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
            quote(view, row, ours);

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
                mark.setImageDrawable(new Glyph(Glyph.mark(state), metaColor, getColor(R.color.bubble_ours),
                        getColor(R.color.failed_ground), getColor(R.color.failed_mark)));
                mark.setContentDescription(Ui.state(ChatActivity.this, state));
            }

            LinearLayout chips = (LinearLayout) view.findViewById(R.id.chips);
            chips.removeAllViews();
            for (Map<String, Object> chip : Core.list(row, "chips")) {
                chips.addView(pill(chip));
            }
            chips.setVisibility(chips.getChildCount() > 0 ? View.VISIBLE : View.GONE);
            return view;
        }

        /**
         * A reply's quote above the body: a stripe, who wrote the message
         * answered, its first line; in the bubble's meta colour.
         */
        private void quote(View view, Map<String, Object> row, boolean ours) {
            View quote = view.findViewById(R.id.quote);
            boolean has = row.get("quote") != null;
            quote.setVisibility(has ? View.VISIBLE : View.GONE);
            if (!has) {
                return;
            }
            int meta = getColor(ours ? R.color.bubble_ours_meta : R.color.bubble_theirs_meta);
            view.findViewById(R.id.quote_stripe).setBackgroundColor(meta);
            TextView who = (TextView) view.findViewById(R.id.quote_who);
            Object quoteOurs = row.get("quote_ours");
            who.setVisibility(quoteOurs == null ? View.GONE : View.VISIBLE);
            who.setText(Boolean.TRUE.equals(quoteOurs) ? getString(R.string.quote_you) : title.getText());
            who.setTextColor(meta);
            TextView text = (TextView) view.findViewById(R.id.quote_text);
            text.setText(Core.str(row, "quote"));
            text.setTextColor(meta);
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

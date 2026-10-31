package network.leviculum.loquax;

import android.app.Activity;
import android.content.Context;
import android.content.Intent;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.text.Editable;
import android.text.TextWatcher;
import android.view.MenuItem;
import android.view.View;
import android.view.ViewGroup;
import android.view.inputmethod.InputMethodManager;
import android.widget.AdapterView;
import android.widget.BaseAdapter;
import android.widget.EditText;
import android.widget.ImageButton;
import android.widget.ImageView;
import android.widget.ListView;
import android.widget.PopupMenu;
import android.widget.TextView;
import java.util.ArrayList;
import java.util.List;
import java.util.Locale;
import java.util.Map;

/**
 * The conversation list, after a modern messenger's overview (1130): our
 * avatar, the app's name, search and a menu in the bar; each row an avatar,
 * the name, two lines of the last message, the time, and the unread count
 * or our last message's delivery mark; newest activity first, as the
 * library orders it, again on every `changed`; a round button for a new one.
 *
 * A long press on a row (1146), after Signal's, marks it and opens its
 * menu: read or unread, pin, mute, archive, delete. The archived
 * conversations are this screen again with ARCHIVED, reached from the
 * menu and from the list's last row; the menu's settings hold the blocked
 * contacts and the service's end.
 */
public class ConversationsActivity extends Activity implements Events.Listener {
    /** The extra that makes this screen the archived view. */
    static final String ARCHIVED = "archived";
    /** Every row the library listed, and those the search leaves. */
    private final List<Map<String, Object>> all = new ArrayList<>();
    private final List<Map<String, Object>> rows = new ArrayList<>();
    private TextView status;
    private TextView title;
    private EditText query;
    private ImageButton search;
    private AvatarView own;
    private Adapter adapter;
    private boolean archivedView;
    private boolean homeClient;
    /** The list's last row: the archived view, with how many. */
    private TextView archivedRow;
    private final ServiceLink link = new ServiceLink(this);
    /** "Jetzt" turns into "1 Min." without a message: the rows redraw twice a minute. */
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
        setContentView(R.layout.activity_conversations);
        status = (TextView) findViewById(R.id.status);
        title = (TextView) findViewById(R.id.title);
        query = (EditText) findViewById(R.id.query);
        own = (AvatarView) findViewById(R.id.avatar);
        archivedView = getIntent().getBooleanExtra(ARCHIVED, false);
        int text = getColor(R.color.text);
        ListView list = (ListView) findViewById(R.id.list);
        archivedRow = new TextView(this);
        archivedRow.setTextColor(getColor(R.color.text_secondary));
        archivedRow.setTextSize(15);
        archivedRow.setGravity(android.view.Gravity.CENTER);
        int pad = Math.round(16 * getResources().getDisplayMetrics().density);
        archivedRow.setPadding(pad, pad, pad, pad);
        archivedRow.setVisibility(View.GONE);
        list.addFooterView(archivedRow, null, true);
        adapter = new Adapter();
        list.setAdapter(adapter);
        list.setEmptyView(findViewById(R.id.empty));
        list.setOnItemClickListener(new AdapterView.OnItemClickListener() {
            @Override
            public void onItemClick(AdapterView<?> parent, View view, int position, long id) {
                if (position >= rows.size()) {
                    archived();
                    return;
                }
                chat(Core.str(rows.get(position), "peer"));
            }
        });
        list.setOnItemLongClickListener(new AdapterView.OnItemLongClickListener() {
            @Override
            public boolean onItemLongClick(AdapterView<?> parent, View view, int position, long id) {
                if (position >= rows.size()) {
                    return false;
                }
                rowMenu(view, rows.get(position));
                return true;
            }
        });
        if (archivedView) {
            title.setText(R.string.archived);
            ((TextView) findViewById(R.id.empty)).setText(R.string.archived_none);
            findViewById(R.id.write).setVisibility(View.GONE);
        }
        search = (ImageButton) findViewById(R.id.search);
        search.setImageDrawable(new Glyph(Glyph.Kind.SEARCH, text));
        search.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                searching(query.getVisibility() != View.VISIBLE);
            }
        });
        query.addTextChangedListener(new TextWatcher() {
            @Override
            public void beforeTextChanged(CharSequence s, int start, int count, int after) {}

            @Override
            public void onTextChanged(CharSequence s, int start, int before, int count) {}

            @Override
            public void afterTextChanged(Editable s) {
                filter();
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
        ImageButton write = (ImageButton) findViewById(R.id.write);
        write.setImageDrawable(new Glyph(Glyph.Kind.PENCIL, getColor(R.color.on_send)));
        write.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                startActivity(new Intent(ConversationsActivity.this, NewConversationActivity.class));
            }
        });
        Asks.once(this);
    }

    /** The bar's menu: pull now, the map, the archived view, the settings. */
    private void showMenu(View anchor) {
        PopupMenu popup = new PopupMenu(this, anchor);
        popup.getMenu().add(0, R.string.refresh, 0, R.string.refresh);
        popup.getMenu().add(0, R.string.map, 1, R.string.map);
        if (!archivedView) {
            popup.getMenu().add(0, R.string.archived, 2, R.string.archived);
        }
        popup.getMenu().add(0, R.string.settings, 3, R.string.settings);
        popup.setOnMenuItemClickListener(new PopupMenu.OnMenuItemClickListener() {
            @Override
            public boolean onMenuItemClick(MenuItem item) {
                int id = item.getItemId();
                if (id == R.string.refresh) {
                    say(Core.pull());
                } else if (id == R.string.map) {
                    startActivity(new Intent(ConversationsActivity.this, MapActivity.class));
                } else if (id == R.string.archived) {
                    archived();
                } else {
                    startActivity(new Intent(ConversationsActivity.this, SettingsActivity.class));
                }
                return true;
            }
        });
        popup.show();
    }

    private void archived() {
        startActivity(new Intent(this, ConversationsActivity.class).putExtra(ARCHIVED, true));
    }

    /**
     * A row's menu, after Signal's long press: the row marked while it is
     * open; read or unread, pin, mute, archive, delete.
     */
    private void rowMenu(final View view, Map<String, Object> row) {
        final String peer = Core.str(row, "peer");
        final String name = Core.str(row, "name");
        final boolean unread = Core.num(row, "unread") > 0;
        final boolean pinned = Core.bool(row, "pinned");
        final boolean muted = Core.bool(row, "muted");
        view.setBackgroundColor(getColor(R.color.selected_row));
        PopupMenu popup = new PopupMenu(this, view, android.view.Gravity.END);
        popup.getMenu().add(0, R.string.mark_read, 0, unread ? R.string.mark_read : R.string.mark_unread);
        if (!archivedView) {
            popup.getMenu().add(0, R.string.pin, 1, pinned ? R.string.unpin : R.string.pin);
        }
        popup.getMenu().add(0, R.string.mute, 2, muted ? R.string.unmute : R.string.mute);
        popup.getMenu().add(0, R.string.archive, 3, archivedView ? R.string.unarchive : R.string.archive);
        popup.getMenu().add(0, R.string.delete, 4, R.string.delete);
        popup.setOnDismissListener(new PopupMenu.OnDismissListener() {
            @Override
            public void onDismiss(PopupMenu menu) {
                view.setBackgroundColor(0);
            }
        });
        final Runnable again = new Runnable() {
            @Override
            public void run() {
                refresh();
            }
        };
        popup.setOnMenuItemClickListener(new PopupMenu.OnMenuItemClickListener() {
            @Override
            public boolean onMenuItemClick(MenuItem item) {
                int id = item.getItemId();
                if (id == R.string.mark_read) {
                    say(Core.unread(peer, !unread));
                } else if (id == R.string.pin) {
                    say(Core.pin(peer, !pinned));
                } else if (id == R.string.mute) {
                    ContactActivity.muteDialog(ConversationsActivity.this, peer, muted, again);
                } else if (id == R.string.archive) {
                    say(Core.archive(peer, !archivedView));
                } else if (id == R.string.delete) {
                    ContactActivity.deleteDialog(ConversationsActivity.this, peer, name,
                            homeClient, again);
                }
                refresh();
                return true;
            }
        });
        popup.show();
    }

    /** The search field in the bar's place of the name, or the name again. */
    private void searching(boolean on) {
        InputMethodManager keyboard = (InputMethodManager) getSystemService(Context.INPUT_METHOD_SERVICE);
        title.setVisibility(on ? View.GONE : View.VISIBLE);
        query.setVisibility(on ? View.VISIBLE : View.GONE);
        search.setImageDrawable(new Glyph(on ? Glyph.Kind.CLOSE : Glyph.Kind.SEARCH, getColor(R.color.text)));
        search.setContentDescription(getString(on ? R.string.close_search : R.string.search));
        if (on) {
            query.requestFocus();
            keyboard.showSoftInput(query, InputMethodManager.SHOW_IMPLICIT);
        } else {
            query.setText("");
            keyboard.hideSoftInputFromWindow(query.getWindowToken(), 0);
        }
    }

    @Override
    public void onBackPressed() {
        if (query.getVisibility() == View.VISIBLE) {
            searching(false);
            return;
        }
        super.onBackPressed();
    }

    private void chat(String peer) {
        startActivity(new Intent(this, ChatActivity.class).putExtra(ChatActivity.PEER, peer));
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
        if (Ui.NOT_STARTED.equals(Core.close())) {
            // Back from the recent apps after the session was stopped: the
            // first screen starts it again.
            startActivity(new Intent(this, MainActivity.class));
            finish();
            return;
        }
        Core.front(true);
        refresh();
        ticker.postDelayed(tick, 30_000);
    }

    @Override
    protected void onPause() {
        ticker.removeCallbacks(tick);
        Core.front(false);
        Events.listen(null);
        super.onPause();
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

    private void refresh() {
        Map<String, Object> answer = archivedView ? Core.archived() : Core.conversations();
        if (answer.containsKey("error")) {
            line(Core.str(answer, "error"));
            return;
        }
        own.show(Core.str(answer, "address"), answer);
        homeClient = Core.bool(answer, "home_client");
        long archived = Core.num(answer, "archived");
        archivedRow.setVisibility(!archivedView && archived > 0 ? View.VISIBLE : View.GONE);
        archivedRow.setText(getString(R.string.archived_count, archived));
        all.clear();
        all.addAll(Core.list(answer, "conversations"));
        filter();
    }

    /** The rows whose name or last line hold what the search field holds, in the library's order. */
    private void filter() {
        String needle = query.getText().toString().trim().toLowerCase(Locale.getDefault());
        rows.clear();
        for (Map<String, Object> row : all) {
            if (needle.isEmpty()
                    || Core.str(row, "name").toLowerCase(Locale.getDefault()).contains(needle)
                    || Ui.preview(this, row).toLowerCase(Locale.getDefault()).contains(needle)) {
                rows.add(row);
            }
        }
        adapter.notifyDataSetChanged();
    }

    @Override
    public void onEvent(Map<String, Object> event) {
        String kind = Core.str(event, "t");
        if ("changed".equals(kind)) {
            refresh();
        } else if ("stopped".equals(kind)) {
            // The notification's stop button, the menu's, or the engine
            // gone: the session is over, and so is the app until it starts
            // again.
            finish();
        } else {
            String line = Ui.eventLine(this, event);
            if (line != null) {
                Ui.showLine(status, line, Ui.stays(event));
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
            long unread = Core.num(row, "unread");
            TextView name = (TextView) view.findViewById(R.id.name);
            // Pinned and muted marks after the name (1146).
            String marks = (Core.bool(row, "pinned") ? " \uD83D\uDCCC" : "")
                    + (Core.bool(row, "muted") ? " \uD83D\uDD15" : "");
            name.setText(Core.str(row, "name") + marks);
            name.setTypeface(null, unread > 0 ? android.graphics.Typeface.BOLD : android.graphics.Typeface.NORMAL);
            ((TextView) view.findViewById(R.id.last)).setText(Ui.preview(ConversationsActivity.this, row));
            TextView at = (TextView) view.findViewById(R.id.at);
            at.setText(Ui.listTime(ConversationsActivity.this, Core.num(row, "at"), Ui.nowSecs()));
            at.setTextColor(getColor(unread > 0 ? R.color.accent : R.color.text_secondary));
            ((AvatarView) view.findViewById(R.id.avatar)).show(Core.str(row, "peer"), row);
            TextView badge = (TextView) view.findViewById(R.id.unread);
            badge.setVisibility(unread > 0 ? View.VISIBLE : View.GONE);
            badge.setText(String.valueOf(unread));
            // Under the time either the count or, ours being the last, its
            // mark: the ticks cut into the list's ground.
            String state = Core.bool(row, "ours") ? Core.str(row, "state") : "";
            ImageView mark = (ImageView) view.findViewById(R.id.mark);
            mark.setVisibility(unread == 0 && !state.isEmpty() ? View.VISIBLE : View.GONE);
            if (!state.isEmpty()) {
                mark.setImageDrawable(new Glyph(Glyph.mark(state), getColor(R.color.text_secondary),
                        getColor(R.color.background), getColor(R.color.list_failed_ground),
                        getColor(R.color.list_failed_mark)));
                mark.setContentDescription(Ui.state(ConversationsActivity.this, state));
            }
            return view;
        }
    }
}

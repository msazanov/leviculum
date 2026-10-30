package network.leviculum.loquax;

import android.app.Activity;
import android.content.Intent;
import android.os.Bundle;
import android.view.Gravity;
import android.view.View;
import android.view.ViewGroup;
import android.widget.Button;
import android.widget.ImageButton;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;
import java.util.List;
import java.util.Map;

/**
 * The app's settings (1146), from the list's menu: the blocked contacts,
 * each with its avatar, name and short hash and an "Unblock" that brings
 * its conversation back on every device of the home; and the service's
 * end, which the list's menu offered before.
 *
 * Above them the mailbox (1152): for a mailbox on this phone, "Back up
 * your mailbox". Below them the advanced settings, the only place the app
 * names a Propagation Node.
 */
public class SettingsActivity extends Activity implements Events.Listener {
    private LinearLayout body;
    private final ServiceLink link = new ServiceLink(this);

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        LinearLayout screen = new LinearLayout(this);
        screen.setOrientation(LinearLayout.VERTICAL);
        screen.setFitsSystemWindows(true);
        screen.setBackgroundColor(getColor(R.color.background));
        LinearLayout bar = new LinearLayout(this);
        bar.setGravity(Gravity.CENTER_VERTICAL);
        bar.setPadding(dp(4), 0, dp(4), 0);
        ImageButton back = new ImageButton(this);
        back.setImageDrawable(new Glyph(Glyph.Kind.BACK, getColor(R.color.text)));
        back.setBackgroundResource(android.R.color.transparent);
        back.setContentDescription(getString(R.string.back));
        back.setPadding(dp(12), dp(12), dp(12), dp(12));
        back.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                finish();
            }
        });
        bar.addView(back, new LinearLayout.LayoutParams(dp(48), dp(48)));
        TextView title = text(getString(R.string.settings), 20, R.color.text);
        title.setPadding(dp(12), 0, 0, 0);
        bar.addView(title);
        screen.addView(bar, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, dp(64)));
        ScrollView scroll = new ScrollView(this);
        body = new LinearLayout(this);
        body.setOrientation(LinearLayout.VERTICAL);
        scroll.addView(body);
        screen.addView(scroll, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, 0, 1));
        setContentView(screen);
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
        refresh();
    }

    @Override
    protected void onPause() {
        Events.listen(null);
        super.onPause();
    }

    @Override
    public void onEvent(Map<String, Object> event) {
        String kind = Core.str(event, "t");
        if ("changed".equals(kind)) {
            refresh();
        } else if ("stopped".equals(kind)) {
            finish();
        }
    }

    private void refresh() {
        body.removeAllViews();
        mailbox();
        TextView header = text(getString(R.string.blocked_contacts), 14, R.color.accent);
        header.setPadding(dp(16), dp(16), dp(16), dp(4));
        body.addView(header);
        Map<String, Object> answer = Core.blocked();
        List<Map<String, Object>> rows = Core.list(answer, "conversations");
        if (answer.containsKey("error")) {
            body.addView(padded(text(Core.str(answer, "error"), 15, R.color.text_secondary)));
        } else if (rows.isEmpty()) {
            body.addView(padded(text(getString(R.string.blocked_none), 15, R.color.text_secondary)));
        }
        for (Map<String, Object> row : rows) {
            body.addView(blocked(row));
        }
        TextView stop = text(getString(R.string.stop_service), 16, R.color.text);
        stop.setPadding(dp(16), dp(24), dp(16), dp(16));
        stop.setBackgroundResource(android.R.drawable.list_selector_background);
        stop.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                // What the notification's stop button does: the service
                // ends the session, `stopped` closes the screens.
                startService(new Intent(SettingsActivity.this, LoquaxService.class)
                        .setAction(LoquaxService.STOP));
            }
        });
        body.addView(stop, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
        TextView advanced = text(getString(R.string.advanced), 14, R.color.accent);
        advanced.setPadding(dp(16), dp(16), dp(16), dp(4));
        body.addView(advanced);
        TextView node = text(getString(R.string.propagation_node), 16, R.color.text);
        node.setPadding(dp(16), dp(8), dp(16), 0);
        body.addView(node);
        body.addView(padded(text(getString(R.string.propagation_node_text), 14,
                R.color.text_secondary)));
    }

    /** Where the mailbox is, and its backup when it is on this phone. */
    private void mailbox() {
        TextView header = text(getString(R.string.mailbox), 14, R.color.accent);
        header.setPadding(dp(16), dp(16), dp(16), dp(4));
        body.addView(header);
        boolean phone = Core.bool(Core.me(), "here");
        body.addView(padded(text(getString(phone ? R.string.mailbox_here : R.string.mailbox_away),
                15, R.color.text_secondary)));
        if (!phone) {
            return;
        }
        TextView backup = text(getString(R.string.backup_title), 16, R.color.text);
        backup.setPadding(dp(16), dp(12), dp(16), dp(12));
        backup.setBackgroundResource(android.R.drawable.list_selector_background);
        backup.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                startActivity(new Intent(SettingsActivity.this, BackupActivity.class));
            }
        });
        body.addView(backup, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
    }

    /** One blocked peer: its avatar and name as every screen shows it, and "Unblock". */
    private View blocked(Map<String, Object> row) {
        final String peer = Core.str(row, "peer");
        LinearLayout line = new LinearLayout(this);
        line.setGravity(Gravity.CENTER_VERTICAL);
        line.setPadding(dp(16), dp(8), dp(8), dp(8));
        AvatarView avatar = new AvatarView(this, null);
        avatar.show(peer, row);
        line.addView(avatar, new LinearLayout.LayoutParams(dp(40), dp(40)));
        TextView name = text(Core.str(row, "name"), 16, R.color.text);
        name.setPadding(dp(16), 0, dp(8), 0);
        line.addView(name, new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1));
        Button unblock = new Button(this);
        unblock.setText(R.string.unblock);
        unblock.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                Core.block(peer, false);
                refresh();
            }
        });
        line.addView(unblock);
        return line;
    }

    private View padded(TextView view) {
        view.setPadding(dp(16), dp(8), dp(16), dp(8));
        return view;
    }

    private TextView text(String words, float sp, int colour) {
        TextView view = new TextView(this);
        view.setText(words);
        view.setTextSize(sp);
        view.setTextColor(getColor(colour));
        return view;
    }

    private int dp(int value) {
        return Math.round(value * getResources().getDisplayMetrics().density);
    }
}

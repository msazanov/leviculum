package network.leviculum.loquax;

import android.content.Context;
import android.graphics.Canvas;
import android.graphics.Color;
import android.graphics.Paint;
import android.graphics.Typeface;
import android.util.AttributeSet;
import android.view.View;
import java.util.Map;

/**
 * A peer's avatar: a circle with its initials (1129). The colours are the
 * peer's icon appearance when it sent one, else one of eight picked by the
 * address, each with white letters at 5:1 or more.
 */
public class AvatarView extends View {
    /** White on each is 5.1:1 to 6.0:1 (the contrast table in 1129's report). */
    private static final int[] PALETTE = {
        0xff8e4b8a, 0xff3d6fb6, 0xffb0503a, 0xff2f7a6e,
        0xff7a5fb8, 0xffa2526a, 0xff5b6b2f, 0xff8a6420,
    };

    private final Paint circle = new Paint(Paint.ANTI_ALIAS_FLAG);
    private final Paint letters = new Paint(Paint.ANTI_ALIAS_FLAG);
    private String initials = "";

    public AvatarView(Context context, AttributeSet attrs) {
        super(context, attrs);
        letters.setTextAlign(Paint.Align.CENTER);
        letters.setTypeface(Typeface.create(Typeface.DEFAULT, Typeface.BOLD));
    }

    /** Show the peer `peer` (hex) as `row` of the facade describes it. */
    void show(String peer, Map<String, Object> row) {
        initials = Core.str(row, "initials");
        int bg = colour(Core.str(row, "icon_bg"));
        int fg = colour(Core.str(row, "icon_fg"));
        if (bg == 0 || fg == 0) {
            bg = PALETTE[peer.length() >= 2 ? Integer.parseInt(peer.substring(0, 2), 16) % PALETTE.length : 0];
            fg = Color.WHITE;
        }
        circle.setColor(bg);
        letters.setColor(fg);
        setContentDescription(Core.str(row, "name"));
        invalidate();
    }

    /** An `rrggbb` as an opaque colour, 0 for anything else. */
    private static int colour(String hex) {
        if (hex.length() != 6) {
            return 0;
        }
        try {
            return 0xff000000 | Integer.parseInt(hex, 16);
        } catch (NumberFormatException e) {
            return 0;
        }
    }

    @Override
    protected void onDraw(Canvas canvas) {
        float side = Math.min(getWidth(), getHeight());
        float cx = getWidth() / 2f;
        float cy = getHeight() / 2f;
        canvas.drawCircle(cx, cy, side / 2f, circle);
        letters.setTextSize(side * (initials.codePointCount(0, initials.length()) > 1 ? 0.38f : 0.45f));
        Paint.FontMetrics m = letters.getFontMetrics();
        canvas.drawText(initials, cx, cy - (m.ascent + m.descent) / 2f, letters);
    }
}

package network.leviculum.loquax;

import android.content.Context;
import android.text.Layout;
import android.util.AttributeSet;
import android.view.View;
import android.view.ViewGroup;
import android.widget.TextView;

/**
 * A bubble: the body, and in its bottom right corner the time and the
 * delivery mark (1129). They sit on the body's last line when it leaves
 * room for them, as in a modern messenger, and on a line of their own
 * below otherwise. A reply's quote (1137), the third child when there
 * is one, sits above the body across the bubble's width. The bubble is
 * at most {@link #setMaxWidth} wide.
 */
public class BubbleLayout extends ViewGroup {
    private int maxWidth = Integer.MAX_VALUE;
    private int gap;
    private boolean below;
    /** The quote's height with its gap to the body, 0 without one. */
    private int quoteH;

    public BubbleLayout(Context context, AttributeSet attrs) {
        super(context, attrs);
        gap = Math.round(8 * context.getResources().getDisplayMetrics().density);
    }

    void setMaxWidth(int px) {
        if (px != maxWidth) {
            maxWidth = px;
            requestLayout();
        }
    }

    @Override
    protected void onMeasure(int widthSpec, int heightSpec) {
        TextView body = (TextView) getChildAt(0);
        View meta = getChildAt(1);
        int pads = getPaddingLeft() + getPaddingRight();
        int size = MeasureSpec.getMode(widthSpec) == MeasureSpec.UNSPECIFIED
                ? maxWidth : Math.min(MeasureSpec.getSize(widthSpec), maxWidth);
        int room = Math.max(0, size - pads);
        int unspecified = MeasureSpec.makeMeasureSpec(0, MeasureSpec.UNSPECIFIED);
        body.measure(MeasureSpec.makeMeasureSpec(room, MeasureSpec.AT_MOST), unspecified);
        int w = body.getMeasuredWidth();
        int h = body.getMeasuredHeight();
        below = false;
        if (meta.getVisibility() != GONE) {
            meta.measure(unspecified, unspecified);
            int mw = meta.getMeasuredWidth();
            int mh = meta.getMeasuredHeight();
            Layout layout = body.getLayout();
            int last = layout == null ? 0 : (int) Math.ceil(
                    layout.getLineWidth(layout.getLineCount() - 1))
                    + body.getPaddingLeft() + body.getPaddingRight();
            if (last + gap + mw <= room) {
                w = Math.max(w, last + gap + mw);
                h = Math.max(h, mh);
            } else {
                below = true;
                w = Math.max(w, mw);
                h += mh;
            }
        }
        View quote = quote();
        quoteH = 0;
        if (quote != null) {
            quote.measure(MeasureSpec.makeMeasureSpec(room, MeasureSpec.AT_MOST), unspecified);
            w = Math.max(w, quote.getMeasuredWidth());
            quote.measure(MeasureSpec.makeMeasureSpec(w, MeasureSpec.EXACTLY), unspecified);
            quoteH = quote.getMeasuredHeight() + gap / 2;
            h += quoteH;
        }
        setMeasuredDimension(w + pads, h + getPaddingTop() + getPaddingBottom());
    }

    /** The quote, when there is one and it shows. */
    private View quote() {
        View quote = getChildCount() > 2 ? getChildAt(2) : null;
        return quote != null && quote.getVisibility() != GONE ? quote : null;
    }

    @Override
    protected void onLayout(boolean changed, int l, int t, int r, int b) {
        View body = getChildAt(0);
        View meta = getChildAt(1);
        int left = getPaddingLeft();
        int top = getPaddingTop();
        int bottom = getHeight() - getPaddingBottom();
        View quote = quote();
        if (quote != null) {
            quote.layout(left, top, left + quote.getMeasuredWidth(), top + quote.getMeasuredHeight());
        }
        int bodyTop = below ? top + quoteH : bottom - body.getMeasuredHeight();
        body.layout(left, bodyTop, left + body.getMeasuredWidth(), bodyTop + body.getMeasuredHeight());
        if (meta.getVisibility() != GONE) {
            int right = getWidth() - getPaddingRight();
            meta.layout(right - meta.getMeasuredWidth(), bottom - meta.getMeasuredHeight(),
                    right, bottom);
        }
    }
}

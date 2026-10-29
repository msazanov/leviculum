package network.leviculum.loquax;

import android.content.Context;
import android.content.res.Configuration;
import android.graphics.Bitmap;
import android.graphics.Canvas;
import android.graphics.Paint;
import android.view.GestureDetector;
import android.view.MotionEvent;
import android.view.ScaleGestureDetector;
import android.view.View;
import java.nio.ByteBuffer;
import java.util.Map;

/**
 * The map, drawn by the library (loquax-android/src/map.rs) and hosted
 * here as a bitmap: Rust draws the regions, the names and the pins, this
 * view only shows the last frame and says what the finger did.
 *
 * One finger pans, a pinch or a double tap zooms. The finger's gestures
 * move and scale the last frame on screen at once, and a new frame is
 * asked for as soon as the frame thread is free, so frames come at the
 * rate the renderer goes while the finger moves (1131): each is drawn a
 * quarter screen past every edge, so a pan first reveals drawn map, at the
 * zoom step nearest the pinch's live scale, and without the map's names
 * until the finger lets go. A frame that lands is placed by where the
 * view it shows lies in the view the finger made, so the map stays under
 * the finger; at rest the next frame snaps to its zoom step. The request
 * is built when the thread is free, from the gestures up to then, so a
 * newer view supersedes the ones it passed and nothing is lost.
 */
final class MapView extends View {
    /** What a frame said, on the UI thread. */
    interface Listener {
        void onShown(Map<String, Object> shown);
    }

    /**
     * The frame's margin past each edge, as a share of the screen: a
     * quarter drew a 1080 x 2400 frame moving at 64 ms on the host, half
     * at 111 ms (1131), and a smaller frame that lands sooner leaves less
     * of a pan to cover.
     */
    private static final float OVERSCAN = 0.25f;

    private final String dir;
    private final Listener listener;
    private final Paint paint = new Paint(Paint.FILTER_BITMAP_FLAG);
    private final GestureDetector gestures;
    private final ScaleGestureDetector pinch;

    // Everything below is guarded by `this`: the UI thread writes the
    // gestures, the frame thread reads them and writes the frame.
    private Bitmap bitmap;
    /** The bitmaps shown before it, reused once the screen let them go. */
    private Bitmap retired;
    private Bitmap spare;
    /** The frame's margins: the view's top left corner is this pixel of it. */
    private int marginX;
    private int marginY;
    /** The frame was drawn while the finger moved: it has no names. */
    private boolean bare;
    /** The last frame's view: absent before the first. */
    private Long x;
    private Long y;
    private Long zoom;
    private String selected;
    /**
     * Where the frame is on screen: a pixel p of its view is at
     * scale * p + (offX, offY). The gestures change this at once; a frame
     * that lands is placed by it.
     */
    private float scale = 1f;
    private float offX;
    private float offY;
    /** A finger is on the map. */
    private boolean touching;
    private Integer tapX;
    private Integer tapY;
    private boolean fit;
    private boolean load;
    private Long atLat;
    private Long atLon;
    /** A frame is owed: the view changed since the last request. */
    private boolean owed;
    private Thread frames;

    MapView(Context context, String dir, Listener listener) {
        super(context);
        this.dir = dir;
        this.listener = listener;
        gestures = new GestureDetector(context, new GestureDetector.SimpleOnGestureListener() {
            @Override
            public boolean onDown(MotionEvent e) {
                return true;
            }

            @Override
            public boolean onScroll(MotionEvent a, MotionEvent b, float dx, float dy) {
                synchronized (MapView.this) {
                    offX -= dx;
                    offY -= dy;
                }
                invalidate();
                ask();
                return true;
            }

            @Override
            public boolean onSingleTapConfirmed(MotionEvent e) {
                synchronized (MapView.this) {
                    tapX = Math.round((e.getX() - offX) / scale);
                    tapY = Math.round((e.getY() - offY) / scale);
                }
                ask();
                return true;
            }

            @Override
            public boolean onDoubleTap(MotionEvent e) {
                zoomAbout(2f, e.getX(), e.getY());
                return true;
            }
        });
        pinch = new ScaleGestureDetector(context, new ScaleGestureDetector.SimpleOnScaleGestureListener() {
            @Override
            public boolean onScale(ScaleGestureDetector detector) {
                zoomAbout(detector.getScaleFactor(), detector.getFocusX(), detector.getFocusY());
                return true;
            }
        });
    }

    /** Scale what is on screen by `by` about the screen point (fx, fy). */
    private void zoomAbout(float by, float fx, float fy) {
        synchronized (this) {
            scale *= by;
            offX = by * (offX - fx) + fx;
            offY = by * (offY - fy) + fy;
        }
        invalidate();
        ask();
    }

    /**
     * Centre on a place, degrees times a million, as a message's fix opens
     * the map, with the pin of `peer` selected when it is not null.
     */
    synchronized void centreOn(long latE6, long lonE6, String peer) {
        atLat = latE6;
        atLon = lonE6;
        if (peer != null) {
            selected = peer;
        }
    }

    /** Every pin in view. */
    void fitPins() {
        synchronized (this) {
            fit = true;
        }
        ask();
    }

    /** Load the region of the view. */
    void loadArea() {
        synchronized (this) {
            load = true;
        }
        ask();
    }

    /** Draw the view anew: the pins changed, or a region is loading. */
    void ask() {
        synchronized (this) {
            owed = true;
            notifyAll();
        }
    }

    @Override
    public boolean onTouchEvent(MotionEvent event) {
        pinch.onTouchEvent(event);
        gestures.onTouchEvent(event);
        int action = event.getActionMasked();
        if (action == MotionEvent.ACTION_DOWN) {
            synchronized (this) {
                touching = true;
            }
        } else if (action == MotionEvent.ACTION_UP || action == MotionEvent.ACTION_CANCEL) {
            boolean again;
            synchronized (this) {
                touching = false;
                // The names come back, and the zoom snaps to its step.
                again = bare || scale != 1f || offX != 0 || offY != 0;
            }
            if (again) {
                ask();
            }
        }
        return true;
    }

    @Override
    protected void onSizeChanged(int w, int h, int oldw, int oldh) {
        super.onSizeChanged(w, h, oldw, oldh);
        ask();
    }

    @Override
    protected void onAttachedToWindow() {
        super.onAttachedToWindow();
        frames = new Thread(new Runnable() {
            @Override
            public void run() {
                drawFrames();
            }
        }, "loquax-map");
        frames.start();
    }

    @Override
    protected void onDetachedFromWindow() {
        if (frames != null) {
            frames.interrupt();
            frames = null;
        }
        super.onDetachedFromWindow();
    }

    @Override
    protected synchronized void onDraw(Canvas canvas) {
        if (bitmap == null) {
            return;
        }
        canvas.save();
        canvas.translate(offX, offY);
        canvas.scale(scale, scale);
        canvas.drawBitmap(bitmap, -marginX, -marginY, paint);
        canvas.restore();
    }

    private boolean dark() {
        int night = getResources().getConfiguration().uiMode & Configuration.UI_MODE_NIGHT_MASK;
        return night == Configuration.UI_MODE_NIGHT_YES;
    }

    /** A bitmap of `w` x `h` for the next frame: the spare when it fits. */
    private Bitmap canvasFor(int w, int h) {
        synchronized (this) {
            Bitmap reuse = spare;
            spare = null;
            if (reuse != null && reuse.getWidth() == w && reuse.getHeight() == h) {
                return reuse;
            }
        }
        return Bitmap.createBitmap(w, h, Bitmap.Config.ARGB_8888);
    }

    /** The frame thread: wait until a frame is owed, ask the library, show it. */
    private void drawFrames() {
        while (!Thread.currentThread().isInterrupted()) {
            Map<String, Object> request;
            boolean moving;
            int w;
            int h;
            synchronized (this) {
                while (!owed || getWidth() == 0 || getHeight() == 0) {
                    try {
                        wait();
                    } catch (InterruptedException e) {
                        return;
                    }
                }
                owed = false;
                moving = touching;
                w = getWidth();
                h = getHeight();
                // The frame's pixel under the screen's centre becomes the
                // new frame's centre, at the zoom step nearest the scale.
                float cx = w / 2f;
                float cy = h / 2f;
                int panX = Math.round((cx - offX) / scale - cx);
                int panY = Math.round((cy - offY) / scale - cy);
                int steps = Math.round((float) (Math.log(scale) / Math.log(2)));
                request = Core.map("dir", dir,
                        "w", (long) w, "h", (long) h,
                        "margin_x", (long) Math.round(w * OVERSCAN),
                        "margin_y", (long) Math.round(h * OVERSCAN),
                        "moving", moving,
                        "density_milli", (long) Math.round(getResources().getDisplayMetrics().density * 1000),
                        "dark", dark(),
                        "pan_x", (long) panX, "pan_y", (long) panY,
                        "zoom_by", (long) steps,
                        "about_x", (long) (w / 2), "about_y", (long) (h / 2),
                        "fit", fit, "load", load);
                if (x != null) {
                    request.put("x", x);
                    request.put("y", y);
                    request.put("zoom", zoom);
                }
                if (selected != null) request.put("selected", selected);
                if (tapX != null) {
                    request.put("tap_x", (long) tapX);
                    request.put("tap_y", (long) tapY);
                }
                if (atLat != null) {
                    request.put("at_lat_e6", atLat);
                    request.put("at_lon_e6", atLon);
                }
                fit = false;
                load = false;
                tapX = null;
                tapY = null;
                atLat = null;
                atLon = null;
            }
            final Core.MapFrame frame = Core.renderMap(request);
            if (frame.pixels != null) {
                int fw = (int) Core.num(frame.shown, "w");
                int fh = (int) Core.num(frame.shown, "h");
                Bitmap next = canvasFor(fw, fh);
                next.copyPixelsFromBuffer(ByteBuffer.wrap(frame.pixels, frame.offset, frame.length));
                synchronized (this) {
                    place(frame.shown, w, h, moving);
                    spare = retired;
                    retired = bitmap;
                    bitmap = next;
                    bare = moving;
                    marginX = (int) Core.num(frame.shown, "margin_x");
                    marginY = (int) Core.num(frame.shown, "margin_y");
                    x = Core.num(frame.shown, "x");
                    y = Core.num(frame.shown, "y");
                    zoom = Core.num(frame.shown, "zoom");
                    Object chosen = frame.shown.get("selected");
                    selected = chosen == null ? null : String.valueOf(chosen);
                }
            }
            post(new Runnable() {
                @Override
                public void run() {
                    invalidate();
                    listener.onShown(frame.shown);
                }
            });
            if (Core.bool(frame.shown, "loading")) {
                postDelayed(new Runnable() {
                    @Override
                    public void run() {
                        ask();
                    }
                }, 1500);
            }
        }
    }

    /**
     * Place the frame that landed, `shown`, asked for a `w` x `h` screen:
     * where its view lies in the last frame's, carried through what the
     * fingers did since, so the map stays under them. A frame asked at
     * rest, with no finger down and nothing asked since, snaps: it is
     * shown as drawn, its zoom step and its centre. Called holding `this`,
     * before the last frame's view is replaced.
     */
    private void place(Map<String, Object> shown, int w, int h, boolean moving) {
        long z1 = Core.num(shown, "zoom");
        boolean first = x == null || zoom == null || y == null;
        if (first || (!moving && !touching && !owed)) {
            scale = 1f;
            offX = 0;
            offY = 0;
            return;
        }
        // A pixel q of the new view is the pixel p = a q + b of the last
        // one: a the ratio of their scales, b from their centres' distance
        // in the last view's pixels, x across the antimeridian the short way.
        double a = Math.pow(2, zoom - z1);
        double unit = Math.pow(2, 24 - zoom);
        long dx = Core.num(shown, "x") - x;
        dx = ((dx + (1L << 31)) & 0xffffffffL) - (1L << 31);
        long dy = Core.num(shown, "y") - y;
        double bx = (1 - a) * (w / 2.0) + dx / unit;
        double by = (1 - a) * (h / 2.0) + dy / unit;
        offX = (float) (scale * bx + offX);
        offY = (float) (scale * by + offY);
        scale = (float) (scale * a);
    }
}

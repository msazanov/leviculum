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
 * One finger pans, a pinch or a double tap zooms; while the finger moves
 * the last frame is shifted and scaled, and at rest the library draws the
 * view anew. Frames are drawn on a thread of their own, one at a time: the
 * request is built when the thread is free, from the gestures up to then,
 * so a newer view supersedes the ones it passed and nothing is lost.
 */
final class MapView extends View {
    /** What a frame said, on the UI thread. */
    interface Listener {
        void onShown(Map<String, Object> shown);
    }

    private final String dir;
    private final Listener listener;
    private final Paint paint = new Paint(Paint.FILTER_BITMAP_FLAG);
    private final GestureDetector gestures;
    private final ScaleGestureDetector pinch;

    // Everything below is guarded by `this`: the UI thread writes the
    // gestures, the frame thread reads them and writes the frame.
    private Bitmap bitmap;
    /** The last frame's view: absent before the first. */
    private Long x;
    private Long y;
    private Long zoom;
    private String selected;
    /** The pixels the content moved since the last frame. */
    private float offX;
    private float offY;
    /** Zoom steps asked since the last frame, about a point of the screen. */
    private int zoomBy;
    private float aboutX;
    private float aboutY;
    /** The live pinch's factor, about its focus. */
    private float pinchBy = 1f;
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
                return true;
            }

            @Override
            public boolean onSingleTapConfirmed(MotionEvent e) {
                synchronized (MapView.this) {
                    tapX = Math.round(e.getX() - offX);
                    tapY = Math.round(e.getY() - offY);
                }
                ask();
                return true;
            }

            @Override
            public boolean onDoubleTap(MotionEvent e) {
                synchronized (MapView.this) {
                    zoomBy += 1;
                    aboutX = e.getX();
                    aboutY = e.getY();
                }
                ask();
                return true;
            }
        });
        pinch = new ScaleGestureDetector(context, new ScaleGestureDetector.SimpleOnScaleGestureListener() {
            @Override
            public boolean onScale(ScaleGestureDetector detector) {
                synchronized (MapView.this) {
                    pinchBy *= detector.getScaleFactor();
                    aboutX = detector.getFocusX();
                    aboutY = detector.getFocusY();
                }
                invalidate();
                return true;
            }

            @Override
            public void onScaleEnd(ScaleGestureDetector detector) {
                synchronized (MapView.this) {
                    int steps = Math.round((float) (Math.log(pinchBy) / Math.log(2)));
                    pinchBy = 1f;
                    zoomBy += steps;
                }
                ask();
            }
        });
    }

    /** Centre on a place, degrees times a million, as a report's fix opens the map. */
    synchronized void centreOn(long latE6, long lonE6) {
        atLat = latE6;
        atLon = lonE6;
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
        if ((action == MotionEvent.ACTION_UP || action == MotionEvent.ACTION_CANCEL)
                && !pinch.isInProgress()) {
            boolean moved;
            synchronized (this) {
                moved = offX != 0 || offY != 0;
            }
            if (moved) {
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
        float scale = pinchBy * (float) Math.pow(2, zoomBy);
        canvas.scale(scale, scale, aboutX, aboutY);
        canvas.drawBitmap(bitmap, offX, offY, paint);
        canvas.restore();
    }

    private boolean dark() {
        int night = getResources().getConfiguration().uiMode & Configuration.UI_MODE_NIGHT_MASK;
        return night == Configuration.UI_MODE_NIGHT_YES;
    }

    /** The frame thread: wait until a frame is owed, ask the library, show it. */
    private void drawFrames() {
        while (!Thread.currentThread().isInterrupted()) {
            Map<String, Object> request;
            int sentX;
            int sentY;
            int sentZoom;
            synchronized (this) {
                while (!owed || getWidth() == 0 || getHeight() == 0) {
                    try {
                        wait();
                    } catch (InterruptedException e) {
                        return;
                    }
                }
                owed = false;
                sentX = Math.round(offX);
                sentY = Math.round(offY);
                sentZoom = zoomBy;
                request = Core.map("dir", dir,
                        "w", (long) getWidth(), "h", (long) getHeight(),
                        "density_milli", (long) Math.round(getResources().getDisplayMetrics().density * 1000),
                        "dark", dark(),
                        "pan_x", (long) -sentX, "pan_y", (long) -sentY,
                        "zoom_by", (long) sentZoom,
                        "about_x", (long) Math.round(aboutX), "about_y", (long) Math.round(aboutY),
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
                Bitmap next = Bitmap.createBitmap(
                        (int) Core.num(frame.shown, "w"), (int) Core.num(frame.shown, "h"),
                        Bitmap.Config.ARGB_8888);
                next.copyPixelsFromBuffer(ByteBuffer.wrap(frame.pixels, frame.offset, frame.length));
                synchronized (this) {
                    bitmap = next;
                    x = Core.num(frame.shown, "x");
                    y = Core.num(frame.shown, "y");
                    zoom = Core.num(frame.shown, "zoom");
                    Object chosen = frame.shown.get("selected");
                    selected = chosen == null ? null : String.valueOf(chosen);
                    offX -= sentX;
                    offY -= sentY;
                    zoomBy -= sentZoom;
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
}

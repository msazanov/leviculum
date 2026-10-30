package network.leviculum.loquax;

import android.content.Context;
import android.graphics.Bitmap;
import android.graphics.Color;
import android.widget.ImageView;
import java.util.Map;

/**
 * A QR code the library encoded, drawn: `qr_size` squared modules, one byte
 * each, row by row, as the contact screen, the first start's address and
 * the paper backup get them. Black on white with its quiet zone, scaled by
 * a whole factor so every module stays sharp.
 */
final class QrCode {
    private static final int QUIET = 4;

    private QrCode() {}

    /** The code in `answer` (its `qr` and `qr_size`) as a view about `side` pixels wide; null for none. */
    static ImageView view(Context context, Map<String, Object> answer, int side, String described) {
        Bitmap bitmap = bitmap(answer.get("qr"), (int) Core.num(answer, "qr_size"), side);
        if (bitmap == null) {
            return null;
        }
        ImageView view = new ImageView(context);
        view.setImageBitmap(bitmap);
        view.setContentDescription(described);
        return view;
    }

    /** The modules as a picture about `side` pixels wide; null when they are no code. */
    static Bitmap bitmap(Object bytes, int size, int side) {
        if (!(bytes instanceof byte[]) || size == 0 || ((byte[]) bytes).length != size * size) {
            return null;
        }
        byte[] modules = (byte[]) bytes;
        int width = size + 2 * QUIET;
        int[] pixels = new int[width * width];
        for (int i = 0; i < pixels.length; i++) {
            pixels[i] = Color.WHITE;
        }
        for (int y = 0; y < size; y++) {
            for (int x = 0; x < size; x++) {
                if (modules[y * size + x] != 0) {
                    pixels[(y + QUIET) * width + x + QUIET] = Color.BLACK;
                }
            }
        }
        Bitmap small = Bitmap.createBitmap(pixels, width, width, Bitmap.Config.ARGB_8888);
        int scale = Math.max(1, side / width);
        return Bitmap.createScaledBitmap(small, width * scale, width * scale, false);
    }
}

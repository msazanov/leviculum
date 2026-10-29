package network.leviculum.loquax;

import android.content.Context;
import android.graphics.Paint;
import java.io.BufferedReader;
import java.io.IOException;
import java.io.InputStreamReader;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;
import java.util.Locale;

/**
 * loquax's emoji table (1137), the asset `emoji.tsv` that
 * scripts/gen-emoji-table.py makes from Unicode's emoji-test.txt and CLDR's
 * names; loquax::emoji::Table reads the same format in Rust. Read once per
 * process. An emoji the phone's font cannot draw (one newer than its
 * Android) is left out, so the picker shows no empty boxes.
 */
final class Emoji {
    /** One emoji: itself, its English and German names, its skin tones. */
    static final class Entry {
        final String text;
        final String en;
        final String de;
        final String[] variants;

        Entry(String text, String en, String de, String[] variants) {
            this.text = text;
            this.en = en;
            this.de = de;
            this.variants = variants;
        }
    }

    /** A category: its key from the table, and its emoji. */
    static final class Category {
        final String key;
        final List<Entry> entries = new ArrayList<>();

        Category(String key) {
            this.key = key;
        }
    }

    private static List<Category> table;

    private Emoji() {}

    /** The table, the categories in the picker's order; empty when the asset is missing. */
    static synchronized List<Category> table(Context context) {
        if (table != null) {
            return table;
        }
        List<Category> read = new ArrayList<>();
        Paint paint = new Paint();
        try (BufferedReader in = new BufferedReader(new InputStreamReader(
                context.getAssets().open("emoji.tsv"), StandardCharsets.UTF_8))) {
            String line;
            while ((line = in.readLine()) != null) {
                if (line.isEmpty() || line.startsWith("#")) {
                    continue;
                }
                if (line.startsWith("@")) {
                    read.add(new Category(line.substring(1)));
                    continue;
                }
                String[] cols = line.split("\t");
                if (cols.length < 3 || read.isEmpty() || !paint.hasGlyph(cols[0])) {
                    continue;
                }
                String[] variants = cols.length > 3 ? cols[3].split(" ") : new String[0];
                read.get(read.size() - 1).entries.add(new Entry(cols[0], cols[1], cols[2], variants));
            }
        } catch (IOException e) {
            read.clear();
        }
        table = read;
        return table;
    }

    /** Every emoji whose English or German name holds each word of `query`. */
    static List<Entry> search(Context context, String query) {
        List<Entry> found = new ArrayList<>();
        String[] words = query.trim().toLowerCase(Locale.ROOT).split("\\s+");
        if (words.length == 0 || words[0].isEmpty()) {
            return found;
        }
        for (Category category : table(context)) {
            for (Entry entry : category.entries) {
                String names = (entry.en + " " + entry.de).toLowerCase(Locale.ROOT);
                boolean all = true;
                for (String word : words) {
                    all &= names.contains(word);
                }
                if (all) {
                    found.add(entry);
                }
            }
        }
        return found;
    }

    /** The entry of `text`, or of the emoji `text` is a skin tone of; null for none. */
    static Entry find(Context context, String text) {
        for (Category category : table(context)) {
            for (Entry entry : category.entries) {
                if (entry.text.equals(text)) {
                    return entry;
                }
                for (String variant : entry.variants) {
                    if (variant.equals(text)) {
                        return entry;
                    }
                }
            }
        }
        return null;
    }
}

package network.leviculum.loquax;

import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.TimeZone;

/**
 * The library's calls as maps, in schema version 1
 * (loquax-android/src/schema.rs). Thin on purpose: what a message, a send
 * or a mark does is decided in Rust.
 */
final class Core {
    static final long VERSION = 1;

    private Core() {}

    static Map<String, Object> map(Object... pairs) {
        Map<String, Object> map = new LinkedHashMap<>();
        for (int i = 0; i + 1 < pairs.length; i += 2) {
            map.put((String) pairs[i], pairs[i + 1]);
        }
        return map;
    }

    @SuppressWarnings("unchecked")
    private static Map<String, Object> answer(byte[] bytes) {
        if (bytes == null || bytes.length == 0) {
            return map("ok", false, "error", "the library gave no answer");
        }
        Object value = MsgPack.unpack(bytes);
        if (!(value instanceof Map)) {
            return map("ok", false, "error", "the library's answer is no map");
        }
        Map<String, Object> answer = (Map<String, Object>) value;
        if (num(answer, "v") != VERSION) {
            return map("ok", false, "error",
                    "the library speaks schema " + num(answer, "v") + ", this app " + VERSION);
        }
        return answer;
    }

    static String str(Map<String, Object> map, String key) {
        Object value = map.get(key);
        return value == null ? "" : String.valueOf(value);
    }

    static long num(Map<String, Object> map, String key) {
        Object value = map.get(key);
        return value instanceof Number ? ((Number) value).longValue() : 0;
    }

    static boolean bool(Map<String, Object> map, String key) {
        return Boolean.TRUE.equals(map.get(key));
    }

    @SuppressWarnings("unchecked")
    static List<Map<String, Object>> list(Map<String, Object> map, String key) {
        Object value = map.get(key);
        List<Map<String, Object>> out = new ArrayList<>();
        if (value instanceof List) {
            for (Object item : (List<Object>) value) {
                if (item instanceof Map) {
                    out.add((Map<String, Object>) item);
                }
            }
        }
        return out;
    }

    /** Seconds east of UTC, now. */
    static int offset() {
        return TimeZone.getDefault().getOffset(System.currentTimeMillis()) / 1000;
    }

    /** Start; code and server null on a later start. Blocks: not on the UI thread. */
    static Map<String, Object> start(String dir, String server, String code, String name) {
        Map<String, Object> request = map("dir", dir, "offset", (long) offset());
        if (server != null) request.put("server", server);
        if (code != null) request.put("code", code);
        if (name != null) request.put("name", name);
        return answer(Native.start(MsgPack.pack(request)));
    }

    /** One request; the error in words, or null. */
    static String command(Map<String, Object> request) {
        Map<String, Object> done = answer(Native.command(MsgPack.pack(request)));
        return bool(done, "ok") ? null : str(done, "error");
    }

    static String open(String peer) {
        return command(map("t", "open", "peer", peer));
    }

    static String close() {
        return command(map("t", "close"));
    }

    static String send(String peer, String body) {
        return command(map("t", "send", "peer", peer, "body", body));
    }

    static String react(String peer, String target, String content) {
        return command(map("t", "react", "peer", peer, "target", target, "content", content));
    }

    /** A reply to the message `target` (1137): sent with its id and a quote of it. */
    static String reply(String peer, String target, String body) {
        return command(map("t", "reply", "peer", peer, "target", target, "body", body));
    }

    /** A copy of the message `key` of `peer`'s conversation to `to`. */
    static String forward(String peer, long key, String to) {
        return command(map("t", "forward", "peer", peer, "key", key, "to", to));
    }

    /** Delete the messages `keys`, asked already. */
    static String delete(String peer, List<Long> keys) {
        return command(map("t", "delete", "peer", peer, "keys", keys));
    }

    /** An emoji picked into a message: first in the recents. */
    static String used(String emoji) {
        return command(map("t", "used", "emoji", emoji));
    }

    /** A message's detail lines, the terminal's `v` view; null with the error in `error[0]`. */
    @SuppressWarnings("unchecked")
    static List<String> info(String peer, long key, String[] error) {
        Map<String, Object> answer = answer(Native.query(MsgPack.pack(
                map("t", "info", "peer", peer, "key", key))));
        if (!(answer.get("rows") instanceof List)) {
            error[0] = str(answer, "error");
            return null;
        }
        return strings(answer.get("rows"));
    }

    /** The emoji used last, newest first; none without a session. */
    static List<String> recents() {
        return strings(answer(Native.query(MsgPack.pack(map("t", "recents")))).get("emoji"));
    }

    @SuppressWarnings("unchecked")
    private static List<String> strings(Object value) {
        List<String> out = new ArrayList<>();
        if (value instanceof List) {
            for (Object item : (List<Object>) value) {
                out.add(String.valueOf(item));
            }
        }
        return out;
    }

    /**
     * Link `peer` to the contact `copy` describes (Book.copy): its lookup
     * key, `_ID`, name and thumbnail; again to refresh the copy (1139).
     */
    static String link(String peer, Map<String, Object> copy) {
        Map<String, Object> request = map("t", "link", "peer", peer);
        request.putAll(copy);
        return command(request);
    }

    static String unlink(String peer) {
        return command(map("t", "unlink", "peer", peer));
    }

    /** "Not now" on the banner: it does not ask again for `peer`. */
    static String notNow(String peer) {
        return command(map("t", "notnow", "peer", peer));
    }

    /** The user's own name for `peer`, empty to clear it; every device of the home takes it. */
    static String rename(String peer, String name) {
        return command(map("t", "rename", "peer", peer, "name", name));
    }

    /** The address book's IM rows of protocol LXMF (Book.imRows): what loquax knows is linked. */
    static String relink(List<Map<String, Object>> rows) {
        return command(map("t", "relink", "rows", rows));
    }

    /** Which of `candidates` (Book.candidates) `peer`'s announced name may be, best first. */
    static List<Map<String, Object>> suggest(String peer, List<Map<String, Object>> candidates) {
        return list(answer(Native.query(MsgPack.pack(
                map("t", "suggest", "peer", peer, "candidates", candidates)))), "suggestions");
    }

    /** Every address linked to the contact `lookup`. */
    static List<Map<String, Object>> contact(String lookup) {
        return list(answer(Native.query(MsgPack.pack(map("t", "contact", "lookup", lookup)))),
                "addresses");
    }

    /** Every link: peer, lookup, contact_id, name. */
    static List<Map<String, Object>> links() {
        return list(answer(Native.query(MsgPack.pack(map("t", "links")))), "links");
    }

    static String pull() {
        return command(map("t", "pull"));
    }

    /** A screen came to the front or left it: in front, a down watching link pulls every minute. */
    static String front(boolean front) {
        return command(map("t", "front", "front", front));
    }

    static Map<String, Object> conversations() {
        return answer(Native.query(MsgPack.pack(map("t", "conversations"))));
    }

    static Map<String, Object> messages(String peer) {
        return answer(Native.query(MsgPack.pack(map("t", "messages", "peer", peer))));
    }

    /** A map frame: what it shows, and where its pixels are in the library's bytes. */
    static final class MapFrame {
        final Map<String, Object> shown;
        final byte[] pixels;
        final int offset;
        final int length;

        MapFrame(Map<String, Object> shown, byte[] pixels, int offset, int length) {
            this.shown = shown;
            this.pixels = pixels;
            this.offset = offset;
            this.length = length;
        }
    }

    /** One frame of the map (MapView); pixels null when there is none. Not on the UI thread. */
    @SuppressWarnings("unchecked")
    static MapFrame renderMap(Map<String, Object> request) {
        byte[] bytes;
        try {
            bytes = Native.renderMap(MsgPack.pack(request));
        } catch (RuntimeException e) {
            return new MapFrame(map("ok", false, "error", String.valueOf(e.getMessage())), null, 0, 0);
        }
        if (bytes == null || bytes.length < 4) {
            return new MapFrame(map("ok", false, "error", "the library gave no frame"), null, 0, 0);
        }
        int head = ((bytes[0] & 0xff) << 24) | ((bytes[1] & 0xff) << 16)
                | ((bytes[2] & 0xff) << 8) | (bytes[3] & 0xff);
        if (head < 0 || 4 + head > bytes.length) {
            return new MapFrame(map("ok", false, "error", "the library's frame is cut short"), null, 0, 0);
        }
        byte[] described = new byte[head];
        System.arraycopy(bytes, 4, described, 0, head);
        Map<String, Object> shown = answer(described);
        int length = bytes.length - 4 - head;
        long want = num(shown, "w") * num(shown, "h") * 4;
        if (!bool(shown, "ok") || length != want || want == 0) {
            return new MapFrame(shown, null, 0, 0);
        }
        return new MapFrame(shown, bytes, 4 + head, length);
    }

    /** The next event, or null; blocks up to waitMs. */
    static Map<String, Object> nextEvent(int waitMs) {
        byte[] bytes = Native.nextEvent(waitMs);
        return bytes == null ? null : answer(bytes);
    }
}

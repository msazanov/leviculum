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

    /** The next event, or null; blocks up to waitMs. */
    static Map<String, Object> nextEvent(int waitMs) {
        byte[] bytes = Native.nextEvent(waitMs);
        return bytes == null ? null : answer(bytes);
    }
}

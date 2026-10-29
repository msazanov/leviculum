package network.leviculum.loquax;

import android.content.Context;
import java.text.DateFormat;
import java.text.SimpleDateFormat;
import java.util.Calendar;
import java.util.Date;
import java.util.Locale;
import java.util.Map;

/** What the screens share: times and the status line's words. */
final class Ui {
    /** The library's answer to a call without a session (src/lib.rs). */
    static final String NOT_STARTED = "not started";

    private Ui() {}

    /** Whole days from `then` to `now` by the calendar, 0 for the same day. */
    static int daysBetween(long thenSecs, long nowSecs) {
        Calendar a = Calendar.getInstance();
        a.setTimeInMillis(thenSecs * 1000);
        Calendar b = Calendar.getInstance();
        b.setTimeInMillis(nowSecs * 1000);
        int days = 0;
        while (a.get(Calendar.YEAR) < b.get(Calendar.YEAR)
                || a.get(Calendar.DAY_OF_YEAR) < b.get(Calendar.DAY_OF_YEAR)
                && a.get(Calendar.YEAR) == b.get(Calendar.YEAR)) {
            a.add(Calendar.DAY_OF_YEAR, 1);
            days++;
            if (days > 7) {
                return days;
            }
        }
        return days;
    }

    /** A time of day, in the phone's locale: 18:34 or 6:34 PM. */
    static String clock(long unixSecs) {
        return DateFormat.getTimeInstance(DateFormat.SHORT).format(new Date(unixSecs * 1000));
    }

    /** The chat's day separator (1129): today, yesterday, the weekday within a week, then the date. */
    static String day(Context context, long unixSecs, long nowSecs) {
        int days = daysBetween(unixSecs, nowSecs);
        if (days == 0) {
            return context.getString(R.string.today);
        }
        if (days == 1) {
            return context.getString(R.string.yesterday);
        }
        Date at = new Date(unixSecs * 1000);
        if (days < 7) {
            return new SimpleDateFormat("EEEE", Locale.getDefault()).format(at);
        }
        return DateFormat.getDateInstance(DateFormat.MEDIUM).format(at);
    }

    /** Within the last hour, how long ago in words ("59 Min."); null otherwise. */
    static String ago(Context context, long unixSecs, long nowSecs) {
        long secs = nowSecs - unixSecs;
        if (secs < 0 || secs >= 3600) {
            return null;
        }
        if (secs < 60) {
            return context.getString(R.string.just_now);
        }
        return context.getString(R.string.minutes_ago, (int) (secs / 60));
    }

    /** The conversation list's time: ago, today's clock, yesterday, the weekday, the date. */
    static String listTime(Context context, long unixSecs, long nowSecs) {
        if (unixSecs <= 0) {
            return "";
        }
        String ago = ago(context, unixSecs, nowSecs);
        if (ago != null) {
            return ago;
        }
        int days = daysBetween(unixSecs, nowSecs);
        if (days == 0) {
            return clock(unixSecs);
        }
        if (days == 1) {
            return context.getString(R.string.yesterday);
        }
        Date at = new Date(unixSecs * 1000);
        if (days < 7) {
            return new SimpleDateFormat("EEE", Locale.getDefault()).format(at);
        }
        return DateFormat.getDateInstance(DateFormat.SHORT).format(at);
    }

    static long nowSecs() {
        return System.currentTimeMillis() / 1000;
    }

    /** The status line for an event other than `changed`, or null. */
    static String eventLine(Context context, Map<String, Object> event) {
        if (event.containsKey("error")) {
            return Core.str(event, "error");
        }
        String kind = Core.str(event, "t");
        if ("notice".equals(kind)) {
            return Core.str(event, "text");
        }
        if ("sync".equals(kind)) {
            String state = Core.str(event, "state");
            if ("running".equals(state)) {
                return context.getString(R.string.pulling);
            }
            if ("done".equals(state)) {
                return context.getString(R.string.pulled, Core.str(event, "detail"));
            }
            return context.getString(R.string.pull_failed, Core.str(event, "detail"));
        }
        if ("stopped".equals(kind)) {
            String detail = Core.str(event, "detail");
            return detail.isEmpty() ? context.getString(R.string.stopped) : detail;
        }
        return null;
    }
}

package network.leviculum.loquax;

import android.content.Context;
import java.text.DateFormat;
import java.util.Calendar;
import java.util.Date;
import java.util.Map;

/** What the screens share: times and the status line's words. */
final class Ui {
    private Ui() {}

    /** A time of day for today, a date and time before; empty for none. */
    static String when(long unixSecs) {
        if (unixSecs <= 0) {
            return "";
        }
        Date at = new Date(unixSecs * 1000);
        Calendar then = Calendar.getInstance();
        then.setTime(at);
        Calendar now = Calendar.getInstance();
        boolean today = then.get(Calendar.YEAR) == now.get(Calendar.YEAR)
                && then.get(Calendar.DAY_OF_YEAR) == now.get(Calendar.DAY_OF_YEAR);
        DateFormat format = today
                ? DateFormat.getTimeInstance(DateFormat.SHORT)
                : DateFormat.getDateTimeInstance(DateFormat.SHORT, DateFormat.SHORT);
        return format.format(at);
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

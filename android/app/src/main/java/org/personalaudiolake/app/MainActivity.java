package org.personalaudiolake.app;

import android.Manifest;
import android.app.Activity;
import android.content.pm.PackageManager;
import android.os.Build;
import android.os.Bundle;
import android.view.View;
import android.view.WindowManager;
import android.widget.Button;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;

public final class MainActivity extends Activity {
  private static final int PERMISSION = 41;
  private MainViewModel model;
  private MainViewModel.State current;
  private MainViewModel.PermissionIntent requested;
  private TextView status;
  private Button record, send;
  private LinearLayout recordings;

  @Override
  public void onCreate(Bundle state) {
    super.onCreate(state);
    model = AppComposition.create(this);
    setContentView(view());
    model.observe(v -> runOnUiThread(() -> render(v)));
  }

  @Override
  protected void onResume() {
    super.onResume();
    if (model != null) model.refreshQueue();
  }

  private View view() {
    int p = (int) (20 * getResources().getDisplayMetrics().density);
    LinearLayout root = new LinearLayout(this);
    root.setOrientation(LinearLayout.VERTICAL);
    root.setPadding(p, p, p, p);
    root.addView(text(getString(R.string.app_name), 28));
    root.addView(text(getString(R.string.subtitle), 16));
    record =
        button(
            getString(R.string.record),
            v -> {
              if (current != null && current.recording()) model.stopRecording();
              else model.requestRecording();
            });
    record.setTextSize(22);
    record.setMinHeight((int) (72 * getResources().getDisplayMetrics().density));
    root.addView(record);
    status = text("", 16);
    status.setPadding(0, p / 2, 0, p);
    root.addView(status);
    send = button(getString(R.string.send_now), v -> model.requestSend());
    root.addView(send);
    root.addView(text(getString(R.string.queue_heading), 20));
    recordings = new LinearLayout(this);
    recordings.setOrientation(LinearLayout.VERTICAL);
    root.addView(recordings);
    ScrollView scroll = new ScrollView(this);
    scroll.addView(root);
    return scroll;
  }

  private void render(MainViewModel.State state) {
    current = state;
    record.setText(state.recording() ? R.string.stop_and_send : R.string.record);
    record.setEnabled(!state.busy());
    send.setEnabled(!state.busy() && state.pendingUploads() > 0);
    status.setText(
        state.pendingUploads() == 0
            ? state.status()
            : getResources()
                .getQuantityString(
                    R.plurals.sync_status,
                    state.pendingUploads(),
                    state.status(),
                    state.pendingUploads()));
    if (state.recording() || state.busy())
      getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
    else getWindow().clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
    recordings.removeAllViews();
    if (state.recordings().isEmpty()) recordings.addView(text(getString(R.string.queue_empty), 15));
    else
      for (MainViewModel.QueueRow row : state.recordings()) {
        TextView v =
            text(
                getString(
                    R.string.queue_row, row.name(), row.timestamp(), row.status(), row.detail()),
                15);
        v.setPadding(0, 16, 0, 16);
        recordings.addView(v);
      }
    MainViewModel.PermissionIntent intent = state.permissionIntent();
    if (intent != null && !intent.equals(requested)) {
      requested = intent;
      String[] permissions = permissions(intent.permission());
      boolean granted = true;
      for (String permission : permissions)
        granted &= checkSelfPermission(permission) == PackageManager.PERMISSION_GRANTED;
      if (granted) {
        requested = null;
        model.permissionResult(intent, true);
      } else requestPermissions(permissions, PERMISSION);
    }
  }

  private String[] permissions(MainViewModel.Permission permission) {
    if (permission == MainViewModel.Permission.MICROPHONE)
      return new String[] {Manifest.permission.RECORD_AUDIO};
    if (Build.VERSION.SDK_INT >= 31)
      return new String[] {
        Manifest.permission.BLUETOOTH_ADVERTISE, Manifest.permission.BLUETOOTH_CONNECT
      };
    return new String[0];
  }

  @Override
  public void onRequestPermissionsResult(int code, String[] permissions, int[] results) {
    super.onRequestPermissionsResult(code, permissions, results);
    if (code != PERMISSION || requested == null) return;
    boolean granted = results.length == permissions.length;
    for (int value : results) granted &= value == PackageManager.PERMISSION_GRANTED;
    MainViewModel.PermissionIntent intent = requested;
    requested = null;
    model.permissionResult(intent, granted);
  }

  private TextView text(String value, int size) {
    TextView v = new TextView(this);
    v.setText(value);
    v.setTextSize(size);
    return v;
  }

  private Button button(String value, View.OnClickListener listener) {
    Button v = new Button(this);
    v.setText(value);
    v.setOnClickListener(listener);
    return v;
  }

  @Override
  protected void onDestroy() {
    if (model != null) model.close();
    super.onDestroy();
  }
}

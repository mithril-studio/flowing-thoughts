export interface AppSettings {
  general: {
    window_movable: boolean;
    launch_at_login: boolean;
    show_in_dock: boolean;
  };
  shortcuts: {
    preset: "cmd_shift_space" | "fn" | string;
  };
  microphone: {
    input_device: string;
    noise_suppression_enabled: boolean;
  };
  language: {
    mode: "system" | "en" | string;
  };
  sound: {
    feedback_sounds_enabled: boolean;
  };
  extras: {
    auto_add_to_dictionary: boolean;
    smart_formatting: boolean;
    dangerously_skip_permissions: boolean;
  };
}

export interface AppSettingsUpdateResult {
  settings: AppSettings;
  warnings: string[];
}

export const defaultAppSettings: AppSettings = {
  general: {
    window_movable: true,
    launch_at_login: false,
    show_in_dock: true,
  },
  shortcuts: {
    preset: "cmd_shift_space",
  },
  microphone: {
    input_device: "system_default",
    noise_suppression_enabled: false,
  },
  language: {
    mode: "system",
  },
  sound: {
    feedback_sounds_enabled: false,
  },
  extras: {
    auto_add_to_dictionary: false,
    smart_formatting: true,
    dangerously_skip_permissions: false,
  },
};

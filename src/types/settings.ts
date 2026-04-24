export interface AppSettings {
  general: {
    window_movable: boolean;
    launch_at_login: boolean;
    show_in_dock: boolean;
    window_position: "center" | "top_left" | "top_right" | "bottom_left" | "bottom_right" | string;
  };
  shortcuts: {
    preset: "cmd_shift_space" | "fn" | string;
  };
  microphone: {
    input_device: string;
    noise_suppression_enabled: boolean;
  };
  language: {
    mode: "system" | "en" | "nl" | string;
  };
  sound: {
    feedback_sounds_enabled: boolean;
  };
  extras: {
    auto_add_to_dictionary: boolean;
    smart_formatting: boolean;
    dangerously_skip_permissions: boolean;
    auto_learn_corrections: boolean;
  };
  transcription: {
    provider: "api" | "local";
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
    window_position: "center",
  },
  shortcuts: {
    preset: "fn",
  },
  microphone: {
    input_device: "system_default",
    noise_suppression_enabled: false,
  },
  language: {
    mode: "en",
  },
  sound: {
    feedback_sounds_enabled: false,
  },
  extras: {
    auto_add_to_dictionary: false,
    smart_formatting: false,
    dangerously_skip_permissions: false,
    auto_learn_corrections: true,
  },
  transcription: {
    provider: "api",
  },
};

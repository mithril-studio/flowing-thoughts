export interface AppSettings {
  general: {
    window_movable: boolean;
    launch_at_login: boolean;
    show_in_dock: boolean;
    window_position: "center" | "top_left" | "top_right" | "bottom_left" | "bottom_right" | string;
    theme: "light" | "dark" | "system" | string;
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
    developer_dictionary: boolean;
  };
  transcription: {
    provider: "api" | "local";
    local_model: string;
  };
  coaching: {
    enabled: boolean;
    model: string;
    batch_size: number;
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
    theme: "light",
  },
  shortcuts: {
    preset: "fn",
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
    smart_formatting: false,
    dangerously_skip_permissions: false,
    auto_learn_corrections: true,
    developer_dictionary: true,
  },
  transcription: {
    provider: "local",
    local_model: "whisper-small-q5",
  },
  coaching: {
    enabled: false,
    model: "openai/gpt-4o-mini",
    batch_size: 20,
  },
};

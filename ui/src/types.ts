export interface NodeDef {
  key: string;
  type: string;
  active?: boolean;
  params_info?: ParamInfo[];
  [key: string]: any;
}

export interface DiscreteFloatOption {
  label: string;
  value: number;
}

/// Mirrors `engine::device::ParamType` — flat wire shape via `#[serde(flatten)]`
/// on the parent `ParamInfo`. The `type` field is the discriminator.
export type ParamType =
  | { type: 'ContinuousFloat'; min: number; max: number; default: number; unit?: string; log?: boolean }
  | { type: 'ContinuousInt';   min: number; max: number; default: number; unit?: string }
  | { type: 'DiscreteFloat';   options: DiscreteFloatOption[]; default: number }
  | { type: 'DiscreteBool';    default: boolean; labels?: [string, string] }
  | { type: 'Event';           actions: string[] };

/// Mirrors `engine::device::ParamKind`.
export type ParamKind =
  | { tag: 'ParamMeta'; max_growable_at_runtime: boolean }
  | { tag: 'TypeMeta';     aspect: string }
  | { tag: 'InstanceMeta'; aspect: string };

/// Mirrors `engine::device::ParamInfo` — `ParamType` fields are flattened in.
export type ParamInfo = ParamType & {
  name: string;
  kind: ParamKind;
  visible: boolean;
};
export interface ChainDef {
  input: [number, number];
  output: [number, number];
  nodes: NodeDef[];
}
export interface AppState {
  chains: ChainDef[];
}

export type MidiChannel = number | '*';

export type DeviceDef =
  | { type: 'serial';   dev: string;   baud: number;   fallback: boolean; active: boolean }
  | { type: 'net';      host: string;  port: number;  fallback: boolean; active: boolean }
  | { type: 'midi-in';  dev?: string;  channel: MidiChannel; active: boolean }
  | { type: 'midi-out'; dev?: string;  channel: number; active: boolean };

export type DeviceMap = Record<string, DeviceDef>;

export interface ControlDef {
  target: string;
  ctrl: [number, number];
  param: [number, number];
  round?: number;
}

export interface ControllerDef {
  device: string;
  channel?: number | '*';
  mappings: Record<string, ControlDef>;
}

export interface AudioConfig {
    sample_rate: number;
    buffer_size: number;
    audio_device: string;
    in_channels: number;
    out_channels: number;
}

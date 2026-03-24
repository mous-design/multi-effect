export interface NodeDef {
  key: string;
  type: string;
  active?: boolean;
  [key: string]: any;
}
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

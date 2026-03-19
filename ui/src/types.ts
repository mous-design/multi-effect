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

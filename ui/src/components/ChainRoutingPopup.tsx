import { useState } from 'react';
import { ChainDef } from '../types';
import { t } from '../i18n';
import { Popup } from './Popup';
import { RoutingSelect } from './RoutingSelect';

interface Props {
  chain: ChainDef;
  inChannels: number;
  outChannels: number;
  onApply: (updated: ChainDef) => void;
  onClose: () => void;
}

export function ChainRoutingPopup({ chain, inChannels, outChannels, onApply, onClose }: Props) {
  const [input, setInput] = useState<[number, number]>(chain.input);
  const [output, setOutput] = useState<[number, number]>(chain.output);

  function handleApply() {
    onApply({ ...chain, input, output });
    onClose();
  }

  return (
    <Popup title={t('ui.chain_routing')} onClose={onClose} confirmLabel={t('ui.apply')} onConfirm={handleApply}>
        <RoutingSelect
          input={input} output={output}
          inChannels={inChannels} outChannels={outChannels}
          onChange={(inp, out) => { setInput(inp); setOutput(out); }}
        />
    </Popup>
  );
}

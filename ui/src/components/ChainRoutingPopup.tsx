import { useState } from 'react';
import { ChainDef } from '../types';
import { t } from '../i18n';

interface Props {
  chain: ChainDef;
  inChannels: number;
  outChannels: number;
  onApply: (updated: ChainDef) => void;
  onClose: () => void;
}

function chOptions(n: number) {
  return Array.from({ length: n }, (_, i) => i + 1);
}

export function ChainRoutingPopup({ chain, inChannels, outChannels, onApply, onClose }: Props) {
  const [inL,  setInL]  = useState(chain.input[0]);
  const [inR,  setInR]  = useState(chain.input[1]);
  const [outL, setOutL] = useState(chain.output[0]);
  const [outR, setOutR] = useState(chain.output[1]);

  function handleApply() {
    onApply({ ...chain, input: [inL, inR], output: [outL, outR] });
    onClose();
  }

  return (
    <div className="popup-overlay" onClick={onClose}>
      <div className="popup" onClick={e => e.stopPropagation()}>
        <p className="popup-title">{t('ui.chain_routing')}</p>
        <table className="routing-table">
          <tbody>
            <tr>
              <td className="routing-label">{t('ui.routing_input')}</td>
              <td>
                <label className="routing-ch-label">L</label>
                <select value={inL} onChange={e => setInL(Number(e.target.value))} className="preset-select">
                  {chOptions(inChannels).map(n => <option key={n} value={n}>{n}</option>)}
                </select>
              </td>
              <td>
                <label className="routing-ch-label">R</label>
                <select value={inR} onChange={e => setInR(Number(e.target.value))} className="preset-select">
                  {chOptions(inChannels).map(n => <option key={n} value={n}>{n}</option>)}
                </select>
              </td>
            </tr>
            <tr>
              <td className="routing-label">{t('ui.routing_output')}</td>
              <td>
                <label className="routing-ch-label">L</label>
                <select value={outL} onChange={e => setOutL(Number(e.target.value))} className="preset-select">
                  {chOptions(outChannels).map(n => <option key={n} value={n}>{n}</option>)}
                </select>
              </td>
              <td>
                <label className="routing-ch-label">R</label>
                <select value={outR} onChange={e => setOutR(Number(e.target.value))} className="preset-select">
                  {chOptions(outChannels).map(n => <option key={n} value={n}>{n}</option>)}
                </select>
              </td>
            </tr>
          </tbody>
        </table>
        <div className="popup-actions">
          <button className="popup-confirm" onClick={handleApply}>{t('ui.apply')}</button>
          <button className="popup-cancel" onClick={onClose}>{t('ui.cancel')}</button>
        </div>
      </div>
    </div>
  );
}

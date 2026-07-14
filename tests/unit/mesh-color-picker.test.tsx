import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import { MeshColorPicker } from '../../src/components/Mesh/MeshColorPicker';
import { MESH_PALETTE } from '../../src/lib/meshColors';

describe('MeshColorPicker', () => {
  it('renders a swatch for every palette color plus the custom input', () => {
    render(<MeshColorPicker value={MESH_PALETTE[0].hex} onChange={() => {}} />);
    for (const p of MESH_PALETTE) {
      expect(screen.getByLabelText(`Colour ${p.hex}`)).toBeTruthy();
    }
    expect(screen.getByLabelText('Custom colour')).toBeTruthy();
  });

  it('marks the selected palette swatch as pressed', () => {
    render(<MeshColorPicker value={MESH_PALETTE[2].hex} onChange={() => {}} />);
    expect(screen.getByLabelText(`Colour ${MESH_PALETTE[2].hex}`).getAttribute('aria-pressed')).toBe('true');
    expect(screen.getByLabelText(`Colour ${MESH_PALETTE[0].hex}`).getAttribute('aria-pressed')).toBe('false');
  });

  it('emits the swatch hex on click', () => {
    const onChange = vi.fn();
    render(<MeshColorPicker value={MESH_PALETTE[0].hex} onChange={onChange} />);
    fireEvent.click(screen.getByLabelText(`Colour ${MESH_PALETTE[3].hex}`));
    expect(onChange).toHaveBeenCalledWith(MESH_PALETTE[3].hex);
  });

  it('emits a custom hex from the native color input', () => {
    const onChange = vi.fn();
    render(<MeshColorPicker value={MESH_PALETTE[0].hex} onChange={onChange} />);
    fireEvent.input(screen.getByLabelText('Custom colour'), { target: { value: '#123456' } });
    expect(onChange).toHaveBeenCalledWith('#123456');
  });
});

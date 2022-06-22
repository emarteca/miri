extern "C" {
    fn printer();
}

fn main() {
        unsafe {
                // test void function that prints from C -- call it twice 
                printer();
                printer();
        }
}

extern "C" {
        fn get_num(x: i32) -> i32;
        fn printer();
        fn get_dbl(x: i32) -> f64;
        fn test_stack_spill(a:i32, b:i32, c:i32, d:i32, e:i32, f:i32, g:i32, h:i32, i:i32, j:i32, k:i32, l:i32) -> i32;
}

//extern "C" { pub fn get_num () -> :: std :: os :: raw :: c_int ; }

fn main() {
        let x;
        unsafe {
                    //println!("{}", get_num());
                    printer();
                    x = get_num(1);
//                    let y = get_dbl(x);
    
                    println!("{}", x);
                    printer();
                    let y = test_stack_spill(1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12);
                    println!("{}", y);
        }
        println!("x: {:?}", x);
        println!("rjeiworjweio");
}
